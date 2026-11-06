//! Cases ported from `threadInbox.test.ts`, `threadSnoozed.test.ts`, `threadSort.test.ts`,
//! `projectGrouping.test.ts`, `olderSettledThreads.test.ts` and `Sidebar.logic.test.ts`.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::grouping::*;
use super::inbox::*;
use super::older_settled::*;
use super::settled::*;
use super::sort::*;
use super::status::*;
use super::thread::*;
use super::time::*;
use super::*;
use crate::families::FamilyRead;
use crate::shell::{ProjectShell, ShellSnapshot, ThreadShell};

fn iso(value: &str) -> Ms {
    parse_iso(value).unwrap()
}

fn base(id: &str) -> SidebarThread {
    SidebarThread {
        id: id.to_owned(),
        project_id: "p1".into(),
        title: id.to_owned(),
        model_selection: crate::model::ModelSelection { instance_id:"codex".into(), model:"gpt".into(), options:None },
        branch: None,
        worktree_path: None,
        interaction_mode: "default".into(),
        listed: true,
        latest_run: None,
        runtime: None,
        blocked_on_thread_wait: false,
        latest_user_message_at: None,
        latest_user_authored_message_at: None,
        has_pending_approvals: false,
        has_pending_user_input: false,
        pending_user_input_at: None,
        has_pending_runtime_request: false,
        pending_subagent_request: None,
        pending_guest_message_count: 0,
        has_actionable_proposed_plan: false,
        background_holds_completion: false,
        goal_active: false,
        created_at: Some(iso("2026-06-01T00:00:00.000Z")),
        updated_at: Some(iso("2026-06-01T00:00:00.000Z")),
        archived: false,
        deleted: false,
        settled_override: None,
        settled_at: None,
        unsettled_at: None,
        snoozed_until: None,
        snoozed_at: None,
        pinned: false,
        pin_order_key: None,
        active_order_key: None,
        auto_settle_disabled: false,
        last_visited_at: None,
        regenerating_title: false,
        opens: None,
    }
}

fn runtime(status: &str) -> RuntimeSummary {
    RuntimeSummary {
        status: status.into(),
        active_run_id: None,
        activity_started_at: None,
        last_error: None,
        last_error_class: None,
        updated_at: Some(iso("2026-06-01T00:00:00.000Z")),
    }
}

fn working(id: &str, on: bool) -> SidebarThread {
    SidebarThread { runtime: on.then(|| runtime("running")), ..base(id) }
}

fn run(status: &str, requested: Option<&str>, completed: Option<&str>) -> RunSummary {
    RunSummary { run_id: "r1".into(), status: status.into(), requested_at: requested.map(iso), started_at: None, completed_at: completed.map(iso) }
}

#[test]
fn iso_round_trips_and_rejects_malformed_input() {
    let ms = iso("2026-04-10T12:00:00.000Z");
    assert_eq!(format_iso(ms), "2026-04-10T12:00:00.000Z");
    assert_eq!(parse_iso("2026-04-10T14:00:00+02:00"), Some(ms));
    assert_eq!(parse_iso("2026-04-10T12:00:00.5Z"), Some(ms + 500));
    assert_eq!(parse_iso("not-a-date"), None);
    assert_eq!(parse_iso("2026-02-30T00:00:00Z"), None);
    assert_eq!(format_iso(0), "1970-01-01T00:00:00.000Z");
}

#[test]
fn compact_relative_labels_match_the_sidebar() {
    let now = iso("2026-04-10T12:00:00.000Z");
    assert_eq!(compact_relative_label(now - 30_000, now), "now");
    assert_eq!(compact_relative_label(now + 5_000, now), "now");
    assert_eq!(compact_relative_label(now - 5 * MINUTE_MS, now), "5m");
    assert_eq!(compact_relative_label(now - 3 * HOUR_MS - 1, now), "3h");
    assert_eq!(compact_relative_label(now - 2 * DAY_MS, now), "2d");
    assert_eq!(working_duration_label(45_000), "45s");
    assert_eq!(working_duration_label(61 * MINUTE_MS), "1h 1m");
}

#[test]
fn tracker_stamps_a_return_but_never_on_the_first_observation() {
    let mut tracker = InboxReturnTracker::default();
    let first = tracker.observe(&[working("a", true), working("b", false)], 10_000);
    assert_eq!(first.returned_at(&working("a", true)), None);
    let held = tracker.observe(&[working("a", false), working("b", false)], 10_000);
    assert_eq!(tracker.next_deadline(), Some(12_000));
    let second = tracker.observe(&[working("a", false), working("b", false)], 12_000);
    assert_eq!(second.returned_at(&working("a", false)), Some(12_000));
    assert_eq!(second.returned_at(&working("b", false)), None);
    assert!(held.is_working(&working("a", false)));
    assert_eq!(held.returned_at(&working("a", false)), None);
}

#[test]
fn tracker_holds_placement_then_expires() {
    let mut tracker = InboxReturnTracker::default();
    tracker.observe(&[working("a", true)], 10_000);
    tracker.observe(&[working("a", false)], 10_000);
    assert!(!is_thread_working(&working("a", false)));
    let held = tracker.observe(&[working("a", false)], 11_999);
    assert!(held.is_working(&working("a", false)));
    assert_eq!(held.returned_at(&working("a", false)), None);
    let expired = tracker.observe(&[working("a", false)], 12_000);
    assert!(!expired.is_working(&working("a", false)));
    assert_eq!(expired.returned_at(&working("a", false)), Some(12_000));
    assert_eq!(tracker.next_deadline(), None);
}

#[test]
fn tracker_cancels_a_transient_return_on_resume() {
    let mut tracker = InboxReturnTracker::default();
    tracker.observe(&[working("a", false)], 10_000);
    tracker.observe(&[working("a", true)], 10_000);
    tracker.observe(&[working("a", false)], 10_000);
    tracker.observe(&[working("a", true)], 11_500);
    let resumed = tracker.observe(&[working("a", true)], 12_500);
    assert_eq!(resumed.returned_at(&working("a", true)), None);
    tracker.observe(&[working("a", false)], 12_500);
    assert!(tracker.observe(&[working("a", false)], 14_499).is_working(&working("a", false)));
    assert_eq!(tracker.observe(&[working("a", false)], 14_500).returned_at(&working("a", false)), Some(14_500));
}

#[test]
fn tracker_ends_grace_for_attention_or_manual_placement() {
    let changes: [fn(&mut SidebarThread); 10] = [
        (|t| t.has_pending_approvals = true),
        (|t| t.has_pending_user_input = true),
        (|t| t.pending_subagent_request = Some(SubagentRequest::Approval)),
        (|t| t.pending_guest_message_count = 1),
        (|t| {
            t.interaction_mode = "plan".into();
            t.has_actionable_proposed_plan = true;
        }),
        (|t| t.runtime = Some(RuntimeSummary { last_error: Some("failed".into()), ..runtime("failed") })),
        (|t| t.pinned = true),
        (|t| t.settled_override = Some(SettledOverride::Settled)),
        (|t| t.snoozed_until = Some(iso("2026-06-02T00:00:00Z"))),
        (|t| t.active_order_key = Some("manual-order".into())),
    ];
    for change in changes {
        let mut tracker = InboxReturnTracker::default();
        tracker.observe(&[working("a", true)], 10_000);
        tracker.observe(&[working("a", false)], 10_000);
        let mut changed = working("a", false);
        change(&mut changed);
        let observation = tracker.observe(&[changed.clone()], 10_000);
        assert!(!observation.is_working(&changed));
        assert_eq!(observation.returned_at(&changed), Some(10_000));
        assert_eq!(tracker.next_deadline(), None);
    }
}

#[test]
fn tracker_forgets_removed_threads() {
    let mut tracker = InboxReturnTracker::default();
    tracker.observe(&[working("a", true)], 10_000);
    tracker.observe(&[working("a", false)], 10_000);
    tracker.observe(&[], 10_000);
    assert_eq!(tracker.next_deadline(), None);
}

#[test]
fn inbox_sort_orders_by_actual_return() {
    let mut tracker = InboxReturnTracker::default();
    let a = SidebarThread { created_at: Some(0), ..working("a", false) };
    let b = SidebarThread { created_at: Some(0), ..working("b", false) };
    tracker.observe(&[working("a", true), working("b", true)], 10_000);
    tracker.observe(&[a.clone(), working("b", true)], 10_000);
    tracker.observe(&[a.clone(), b.clone()], 10_500);
    let observation = tracker.observe(&[a.clone(), b.clone()], 12_500);
    let mut list = vec![&a, &b];
    sort_inbox_by_return(&mut list, |thread| observation.returned_at(thread));
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
}

#[test]
fn working_sort_uses_the_last_message_the_user_sent() {
    let sent_first = SidebarThread {
        latest_user_authored_message_at: Some(Some(iso("2026-06-01T01:00:00.000Z"))),
        latest_run: Some(run("running", Some("2026-06-01T04:00:00.000Z"), None)),
        ..working("sent-first", true)
    };
    let sent_last = SidebarThread { latest_user_authored_message_at: Some(Some(iso("2026-06-01T02:00:00.000Z"))), ..working("sent-last", true) };
    let launched = SidebarThread { latest_user_authored_message_at: Some(None), ..working("launched", true) };
    let mut list = vec![&launched, &sent_first, &sent_last];
    sort_working_by_send(&mut list);
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["sent-last", "sent-first", "launched"]);
}

#[test]
fn settled_timestamp_prefers_the_stamp_then_latest_activity() {
    let stamped = SidebarThread {
        settled_at: Some(iso("2026-03-01T00:00:00.000Z")),
        latest_user_message_at: Some(iso("2026-03-05T00:00:00.000Z")),
        ..base("a")
    };
    assert_eq!(settled_timestamp(&stamped), Some(iso("2026-03-01T00:00:00.000Z")));
    let unstamped = SidebarThread {
        latest_user_message_at: Some(iso("2026-03-02T00:00:00.000Z")),
        latest_run: Some(run("completed", Some("2026-03-02T00:00:00.000Z"), Some("2026-03-04T00:00:00.000Z"))),
        ..base("b")
    };
    assert_eq!(settled_timestamp(&unstamped), Some(iso("2026-03-04T00:00:00.000Z")));
    let a = SidebarThread { settled_at: Some(5), ..base("b-thread") };
    let b = SidebarThread { settled_at: Some(5), ..base("a-thread") };
    let mut list = vec![&a, &b, &unstamped];
    sort_settled(&mut list);
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "a-thread", "b-thread"]);
}

#[test]
fn active_order_keeps_new_and_reopened_threads_ahead_of_saved_order() {
    let keyed_a = SidebarThread { active_order_key: Some("m".into()), ..base("keyed-a") };
    let keyed_b = SidebarThread { active_order_key: Some("c".into()), ..base("keyed-b") };
    let new = SidebarThread { created_at: Some(iso("2026-06-03T00:00:00Z")), ..base("new") };
    let reopened = SidebarThread { unsettled_at: Some(iso("2026-06-04T00:00:00Z")), ..base("reopened") };
    let mut list = vec![&keyed_a, &new, &keyed_b, &reopened];
    sort_active_by_order_key(&mut list);
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["reopened", "new", "keyed-b", "keyed-a"]);
}

#[test]
fn pinned_order_puts_keyed_rows_first() {
    let keyless_old = SidebarThread { pinned: true, created_at: Some(1), ..base("old") };
    let keyless_new = SidebarThread { pinned: true, created_at: Some(2), ..base("new") };
    let keyed = SidebarThread { pinned: true, pin_order_key: Some("n".into()), ..base("keyed") };
    let mut list = vec![&keyless_old, &keyless_new, &keyed];
    sort_pinned(&mut list);
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["keyed", "new", "old"]);
}

#[test]
fn pin_order_keys_sort_between_their_neighbors() {
    assert_eq!(pin_order_key_between(None, None).as_deref(), Some("n"));
    let top = pin_order_key_between(None, Some("n")).unwrap();
    assert!(top.as_str() < "n");
    let between = pin_order_key_between(Some("b"), Some("c")).unwrap();
    assert!("b" < between.as_str() && between.as_str() < "c");
    assert_eq!(pin_order_key_between(Some("c"), Some("b")), None);
    assert_eq!(pin_order_key_between(Some("Bad"), None), None);
    let pinned = SidebarThread { pinned: true, pin_order_key: Some("d".into()), ..base("p") };
    assert!(top_of_pinned_order_key([&pinned]).unwrap().as_str() < "d");
}

const NOW: &str = "2026-04-10T12:00:00.000Z";
const SNOOZED_AT: &str = "2026-04-10T09:00:00.000Z";
const FUTURE_WAKE: &str = "2026-04-11T09:00:00.000Z";
const PAST_WAKE: &str = "2026-04-10T10:00:00.000Z";

fn snoozed(until: &str) -> SidebarThread {
    SidebarThread { snoozed_until: parse_iso(until), snoozed_at: Some(iso(SNOOZED_AT)), ..base("t") }
}

#[test]
fn effective_snooze_never_hides_on_malformed_data_and_wakes_for_requests() {
    let now = iso(NOW);
    assert!(!effective_snoozed(&snoozed("not-a-date"), now));
    assert!(effective_snoozed(&snoozed(FUTURE_WAKE), now));
    assert!(!effective_snoozed(&SidebarThread { has_pending_approvals: true, ..snoozed(FUTURE_WAKE) }, now));
    assert!(!effective_snoozed(&SidebarThread { has_pending_user_input: true, ..snoozed(FUTURE_WAKE) }, now));
    assert!(!effective_snoozed(&snoozed(PAST_WAKE), now));
    for kind in ["approval", "auth_refresh", "future_request"] {
        let blocked = present_thread(&serde_json::from_value(wire_thread("request", json!({
            "snoozedUntil": FUTURE_WAKE, "snoozedAt": SNOOZED_AT,
            "pendingRuntimeRequest":{"id":"request", "kind":kind}
        }))).unwrap(), None);
        assert!(!can_snooze(&blocked, now));
        assert!(!effective_snoozed(&blocked, now));
    }
    for block in [
        SidebarThread { has_pending_approvals: true, ..base("approval") },
        SidebarThread { pending_guest_message_count: 1, ..base("guest") },
        SidebarThread { pending_subagent_request: Some(SubagentRequest::UserInput), ..base("child") },
    ] {
        assert!(!can_snooze(&block, now));
    }

    let legacy = present_thread(&serde_json::from_value(wire_thread("legacy", json!({
        "snoozedUntil":FUTURE_WAKE, "snoozedAt":SNOOZED_AT,
        "pendingRuntimeRequest":{"id":"legacy", "kind":"user_input"}
    }))).unwrap(), None);
    assert!(can_snooze(&legacy, now));
    assert!(!effective_snoozed(&legacy, now));
    for (created_at, hidden) in [(json!(SNOOZED_AT), true), (json!("2026-04-10T08:00:00Z"), true), (json!("2026-04-10T09:30:00Z"), false), (Value::Null, false), (json!("invalid"), false)] {
        let question = present_thread(&serde_json::from_value(wire_thread("question", json!({
            "snoozedUntil": FUTURE_WAKE, "snoozedAt": SNOOZED_AT,
            "pendingRuntimeRequest": {"id":"question", "kind":"user_input", "createdAt":created_at, "blocking":false}
        }))).unwrap(), None);
        assert!(can_snooze(&question, now), "existing question: {created_at}");
        assert_eq!(effective_snoozed(&question, now), hidden, "question time: {created_at}");
        assert!(!effective_snoozed(&SidebarThread { snoozed_at: None, ..question.clone() }, now));
        assert_eq!(woke_at(&SidebarThread { snoozed_until: Some(iso(PAST_WAKE)), ..question.clone() }, now), if hidden { Some(iso(PAST_WAKE)) } else if created_at == json!("2026-04-10T09:30:00Z") { Some(iso("2026-04-10T09:30:00Z")) } else { Some(iso(SNOOZED_AT)) });
        if !hidden && created_at == json!("2026-04-10T09:30:00Z") {
            assert_eq!(woke_at(&question, now), Some(iso("2026-04-10T09:30:00Z")));
        }
    }
}

#[test]
fn fresh_failures_wake_but_stale_ones_do_not() {
    let now = iso(NOW);
    let metadata_at = iso("2026-04-10T11:00:00.000Z");
    let failed = |completed| SidebarThread {
        runtime: Some(RuntimeSummary { updated_at: Some(metadata_at), ..runtime("failed") }),
        latest_run: Some(run("failed", None, completed)),
        ..snoozed(FUTURE_WAKE)
    };
    let requests: [fn(&mut SidebarThread); 6] = [
        |t| t.has_pending_approvals = true,
        |t| t.has_pending_user_input = true,
        |t| t.has_pending_runtime_request = true,
        |t| t.pending_subagent_request = Some(SubagentRequest::Approval),
        |t| t.pending_subagent_request = Some(SubagentRequest::UserInput),
        |t| t.pending_guest_message_count = 1,
    ];
    for (completed, failure_wake) in [
        (Some("2026-04-10T08:00:00.000Z"), None),
        (None, None),
        (Some(SNOOZED_AT), None),
        (Some("2026-04-10T09:30:00.000Z"), Some(iso("2026-04-10T09:30:00.000Z"))),
    ] {
        let thread = failed(completed);
        assert_eq!(effective_snoozed(&thread, now), failure_wake.is_none(), "completion: {completed:?}");
        assert_eq!(woke_at(&thread, now), failure_wake, "completion: {completed:?}");
        let timer = SidebarThread { snoozed_until: Some(iso(PAST_WAKE)), ..thread.clone() };
        assert!(!effective_snoozed(&timer, now));
        assert_eq!(woke_at(&timer, now), failure_wake.or(Some(iso(PAST_WAKE))));
        for request in requests {
            let mut blocked = thread.clone();
            request(&mut blocked);
            assert!(!effective_snoozed(&blocked, now));
            assert_eq!(woke_at(&blocked, now), failure_wake.or(Some(metadata_at)));
            blocked.snoozed_until = Some(iso(PAST_WAKE));
            assert_eq!(woke_at(&blocked, now), failure_wake.or(Some(metadata_at)));
        }
    }
    let unstamped = SidebarThread { snoozed_at: None, ..failed(None) };
    assert!(!effective_snoozed(&unstamped, now));
    assert_eq!(woke_at(&unstamped, now), Some(metadata_at));
    for status in ["error", "failed"] {
        for latest_run in [None, Some(run("completed", None, Some("2026-04-10T08:00:00.000Z")))] {
            for updated_at in [None, Some(iso("2026-04-10T08:00:00.000Z")), Some(iso(SNOOZED_AT)), Some(metadata_at)] {
                let legacy = SidebarThread {
                    runtime: Some(RuntimeSummary { updated_at, ..runtime(status) }),
                    latest_run: latest_run.clone(),
                    ..snoozed(FUTURE_WAKE)
                };
                let wake = updated_at.filter(|at| *at > iso(SNOOZED_AT));
                assert_eq!(effective_snoozed(&legacy, now), wake.is_none());
                assert_eq!(woke_at(&legacy, now), wake);
            }
        }
    }
    for (question_at, failure_at, expected) in [
        ("2026-04-10T09:45:00Z", "2026-04-10T09:30:00Z", "2026-04-10T09:45:00Z"),
        ("2026-04-10T09:30:00Z", "2026-04-10T09:45:00Z", "2026-04-10T09:45:00Z"),
        ("2026-04-10T09:30:00Z", "2026-04-10T09:30:00Z", "2026-04-10T09:30:00Z"),
    ] {
        let question = present_thread(&serde_json::from_value(wire_thread("question", json!({
            "snoozedUntil": FUTURE_WAKE, "snoozedAt": SNOOZED_AT,
            "status":"failed", "latestRunId":"failed-run", "latestRunCompletedAt":failure_at,
            "pendingRuntimeRequest":{"id":"question", "kind":"user_input", "createdAt":question_at}
        }))).unwrap(), None);
        assert_eq!(woke_at(&question, now), Some(iso(expected)));
        assert_eq!(woke_at(&SidebarThread { snoozed_until: Some(iso(PAST_WAKE)), ..question }, now), Some(iso(expected)));
    }
    assert_eq!(woke_at(&snoozed(PAST_WAKE), now), Some(iso(PAST_WAKE)));
}

#[test]
fn queued_turn_start_clears_when_adopted() {
    let now = iso(NOW);
    let message_at = "2026-04-10T11:59:00.000Z";
    let queued = SidebarThread { latest_user_message_at: Some(iso(message_at)), ..base("q") };
    assert!(has_queued_turn_start(&queued, now));
    assert!(!can_snooze(&queued, now));
    let adopted = SidebarThread { latest_run: Some(run("running", Some(message_at), None)), ..queued.clone() };
    assert!(!has_queued_turn_start(&adopted, now));
    assert!(!has_queued_turn_start(&queued, now + 3 * MINUTE_MS));
}

fn clock(time: LocalTime) -> String {
    format!("{}:{:02}", time.hour, time.minute)
}

fn menu(now: Ms, presets: &[SnoozePresetSetting]) -> Vec<String> {
    resolve_snooze_presets(presets, now, &FixedZone(0), &clock).into_iter().map(|p| format!("{} {}", p.label, p.when_label)).collect()
}

fn local(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> Ms {
    from_local(year, month, day, hour, minute, &FixedZone(0))
}

#[test]
fn snooze_presets_list_soonest_first_and_name_the_day() {
    let presets = [
        SnoozePresetSetting::At { day: PresetDay::Weekday(1), hour: 18, minute: 0 },
        SnoozePresetSetting::At { day: PresetDay::Tomorrow, hour: 21, minute: 0 },
        SnoozePresetSetting::After { minutes: 360 },
        SnoozePresetSetting::After { minutes: 180 },
    ];
    assert_eq!(menu(local(2026, 4, 8, 20, 0), &presets), ["In 3 hours 23:00", "In 6 hours Thu 2:00", "Tomorrow 21:00", "Next week Mon 18:00"]);
}

#[test]
fn snooze_presets_name_the_real_day_gap_and_dedupe_by_minute() {
    let theo = [
        SnoozePresetSetting::At { day: PresetDay::Tomorrow, hour: 21, minute: 0 },
        SnoozePresetSetting::At { day: PresetDay::Weekday(1), hour: 18, minute: 0 },
        SnoozePresetSetting::At { day: PresetDay::Weekday(5), hour: 17, minute: 0 },
    ];
    assert_eq!(menu(local(2026, 8, 30, 14, 0), &theo), ["Tomorrow 18:00", "Tomorrow 21:00", "Friday 17:00"]);
    assert_eq!(menu(local(2026, 9, 4, 10, 0), &theo), ["Tomorrow 21:00", "Next week Mon 18:00", "Next Friday 17:00"]);
    assert_eq!(menu(local(2026, 8, 30, 7, 1), &DEFAULT_SNOOZE_PRESETS), ["In 1 hour 8:01", "In 3 hours 10:01", "This evening 18:00", "Tomorrow 9:00"]);
    let today_passed = resolve_snooze_presets(
        &[SnoozePresetSetting::At { day: PresetDay::Today, hour: 18, minute: 0 }, SnoozePresetSetting::At { day: PresetDay::Weekday(1), hour: 9, minute: 0 }],
        local(2026, 4, 13, 18, 30),
        &FixedZone(0),
        &clock,
    );
    assert_eq!(today_passed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["at:monday:09:00"]);
    assert_eq!(today_passed[0].snoozed_until, local(2026, 4, 20, 9, 0));
}

#[test]
fn picking_a_duration_starts_it_now_and_a_set_time_keeps_its_wake() {
    let presets = [SnoozePresetSetting::At { day: PresetDay::Today, hour: 18, minute: 30 }, SnoozePresetSetting::After { minutes: 180 }];
    let shown = resolve_snooze_presets(&presets, local(2026, 4, 8, 15, 0), &FixedZone(0), &clock);
    let picked = pick_snooze_preset(&shown, "after:180", local(2026, 4, 8, 15, 30), &presets, &FixedZone(0), &clock);
    assert_eq!(picked.map(|p| p.snoozed_until), Some(local(2026, 4, 8, 18, 30)));
    let shown = resolve_snooze_presets(&DEFAULT_SNOOZE_PRESETS, local(2026, 8, 30, 23, 59), &FixedZone(0), &clock);
    let picked = pick_snooze_preset(&shown, "at:tomorrow:09:00", local(2026, 8, 31, 0, 1), &DEFAULT_SNOOZE_PRESETS, &FixedZone(0), &clock);
    assert_eq!(picked.map(|p| p.snoozed_until), Some(local(2026, 8, 31, 9, 0)));
}

#[test]
fn snooze_presets_follow_a_zone_with_an_offset() {
    let zone = FixedZone(-7 * 3600);
    let now = parse_iso("2026-04-08T20:00:00.000-07:00").unwrap();
    let presets = resolve_snooze_presets(&[SnoozePresetSetting::At { day: PresetDay::Tomorrow, hour: 9, minute: 0 }], now, &zone, &default_time_label);
    assert_eq!(presets[0].snoozed_until, parse_iso("2026-04-09T09:00:00.000-07:00").unwrap());
    assert_eq!(presets[0].when_label, "9:00 AM");
}

#[test]
fn woke_unseen_dismisses_at_its_timestamp() {
    let first = iso("2026-04-10T10:00:00.000Z");
    assert!(is_woke_unseen(Some(first), false, None, None));
    assert!(!is_woke_unseen(Some(first), false, Some(first), None));
    assert!(is_woke_unseen(Some(iso(NOW)), false, Some(first), None));
    assert_eq!(snooze_wake_label(iso(NOW) + 90 * MINUTE_MS, iso(NOW)), "2h");
    assert_eq!(snooze_wake_label(iso(NOW) + 30_000, iso(NOW)), "1m");
}

#[test]
fn sidebar_status_follows_the_web_priority() {
    let preparing = SidebarThread { runtime: Some(runtime("preparing")), goal_active: true, ..base("setup") };
    assert_eq!(top_status(resolve_status(&preparing), preparing.goal_active, false, false).map(|status| status.label), Some("Setting up"));
    assert_eq!(resolve_status(&preparing), ThreadStatus::Preparing);
    for goal_active in [false, true] {
        assert_eq!(top_status(resolve_status(&preparing), goal_active, true, true), Some(TopStatus { label: "Setting up", icon: StatusIcon::Working, tone: StatusTone::Info }));
    }
    assert!(should_recede(resolve_status(&preparing), true, true, false));
    assert!(!should_recede(resolve_status(&preparing), false, false, true));
    assert!(is_thread_working(&preparing));
    for (approval, input, subagent, guest, expected) in [
        (false, false, None, 1, ThreadStatus::Request),
        (false, true, None, 1, ThreadStatus::Input),
        (true, true, None, 1, ThreadStatus::Approval),
        (false, false, Some(SubagentRequest::UserInput), 1, ThreadStatus::Input),
        (true, false, Some(SubagentRequest::UserInput), 1, ThreadStatus::Approval),
        (false, true, Some(SubagentRequest::Approval), 1, ThreadStatus::Approval),
    ] {
        let pending = SidebarThread { has_pending_approvals: approval, has_pending_user_input: input, pending_subagent_request: subagent, pending_guest_message_count: guest, ..preparing.clone() };
        assert_eq!(resolve_status(&pending), expected);
    }
    let mut reverse = preparing.clone();
    for (runtime_status, blocked, expected, label) in [
        ("running", false, ThreadStatus::Working, Some("Goal")),
        ("running", true, ThreadStatus::Waiting, Some("Waiting")),
        ("idle", false, ThreadStatus::Waiting, Some("Waiting")),
        ("completed", false, ThreadStatus::Ready, None),
        ("preparing", true, ThreadStatus::Preparing, Some("Setting up")),
    ] {
        reverse.runtime = Some(runtime(runtime_status));
        reverse.blocked_on_thread_wait = blocked;
        assert_eq!(resolve_status(&reverse), expected);
        assert_eq!(top_status(resolve_status(&reverse), reverse.goal_active, false, false).map(|status| status.label), label);
    }
    for runtime_status in ["queued", "starting", "running", "waiting"] {
        assert_eq!(resolve_status(&SidebarThread { runtime: Some(runtime(runtime_status)), ..base("unchanged") }), ThreadStatus::Working);
    }
    let mut timed = SidebarThread {
        runtime: Some(RuntimeSummary { active_run_id: Some("r1".into()), activity_started_at: Some(Some(100)), ..runtime("preparing") }),
        latest_run: Some(RunSummary { started_at: Some(200), ..run("preparing", Some(NOW), None) }),
        ..base("timer")
    };
    assert_eq!(working_started_at(&timed), Some(100));
    timed.runtime.as_mut().unwrap().activity_started_at = Some(None);
    assert_eq!(working_started_at(&timed), None);
    timed.runtime.as_mut().unwrap().activity_started_at = None;
    assert_eq!(working_started_at(&timed), Some(200));
    timed.latest_run.as_mut().unwrap().started_at = None;
    assert_eq!(working_started_at(&timed), Some(iso(NOW)));
    timed.runtime.as_mut().unwrap().active_run_id = None;
    assert_eq!(working_started_at(&timed), None);
    let mut thread = SidebarThread { runtime: Some(runtime("running")), ..base("s") };
    assert_eq!(resolve_status(&thread), ThreadStatus::Working);
    thread.blocked_on_thread_wait = true;
    assert_eq!(resolve_status(&thread), ThreadStatus::Waiting);
    thread.pending_guest_message_count = 1;
    assert_eq!(resolve_status(&thread), ThreadStatus::Request);
    thread.pending_subagent_request = Some(SubagentRequest::UserInput);
    assert_eq!(resolve_status(&thread), ThreadStatus::Input);
    thread.has_pending_approvals = true;
    assert_eq!(resolve_status(&thread), ThreadStatus::Approval);
    let limited = SidebarThread { runtime: Some(RuntimeSummary { last_error_class: Some("usage_limit".into()), ..runtime("failed") }), ..base("l") };
    assert_eq!(resolve_status(&limited), ThreadStatus::Limited);
    assert_eq!(resolve_status(&SidebarThread { runtime: Some(runtime("failed")), ..base("f") }), ThreadStatus::Failed);
    assert_eq!(resolve_status(&SidebarThread { runtime: Some(runtime("idle")), ..base("i") }), ThreadStatus::Waiting);
    assert_eq!(resolve_status(&base("r")), ThreadStatus::Ready);
}

#[test]
fn unseen_completion_needs_a_visit_older_than_the_completion() {
    let done = SidebarThread { latest_run: Some(run("completed", None, Some("2026-04-10T10:00:00.000Z"))), ..base("d") };
    assert!(!has_unseen_completion(&done));
    assert!(has_unseen_completion(&SidebarThread { last_visited_at: Some(iso("2026-04-10T09:00:00.000Z")), ..done.clone() }));
    assert!(!has_unseen_completion(&SidebarThread { last_visited_at: Some(iso("2026-04-10T11:00:00.000Z")), ..done.clone() }));
    let failed = SidebarThread { latest_run: Some(run("failed", None, Some("2026-04-10T10:00:00.000Z"))), ..base("f") };
    assert!(has_unseen_failure(&failed));
    assert_eq!(top_status(ThreadStatus::Ready, false, false, true).map(|s| s.label), Some("Done"));
    assert_eq!(top_status(ThreadStatus::Working, true, false, false).map(|s| s.label), Some("Goal"));
    assert!(should_recede(ThreadStatus::Ready, false, false, false));
    assert!(!should_recede(ThreadStatus::Ready, true, false, false));
    assert!(!should_recede(ThreadStatus::Input, false, false, false));
    assert!(!should_recede(ThreadStatus::Working, false, false, true));
}

#[test]
fn status_pills_match_the_palette() {
    let pending = SidebarThread { has_pending_approvals: true, ..base("a") };
    assert_eq!(status_pill(&pending).map(|p| p.label), Some("Pending Approval"));
    for runtime_status in ["preparing", "starting", "queued"] {
        let connecting = SidebarThread { runtime: Some(runtime(runtime_status)), ..base("c") };
        assert_eq!(status_pill(&connecting).map(|p| (p.label, p.tone, p.pulse)), Some(("Connecting", PillTone::Working, true)));
    }
    let plan = SidebarThread {
        interaction_mode: "plan".into(),
        has_actionable_proposed_plan: true,
        latest_run: Some(run("completed", None, Some("2026-04-10T10:00:00.000Z"))),
        ..base("p")
    };
    assert_eq!(status_pill(&plan).map(|p| p.label), Some("Plan Ready"));
    assert_eq!(status_pill(&base("idle")), None);
}

fn project(id: &str, title: &str, root: &str, identity: Option<Value>) -> ProjectShell {
    serde_json::from_value(json!({
        "id": id, "title": title, "workspaceRoot": root, "repositoryIdentity": identity,
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z"
    }))
    .unwrap()
}

#[test]
fn project_groups_merge_clones_of_one_repository() {
    let identity = json!({"canonicalKey": "github.com/t3/code", "locator": {}, "name": "code", "displayName": "t3/code", "rootPath": "/a/code"});
    let other = json!({"canonicalKey": "github.com/t3/code", "locator": {}, "name": "code", "displayName": "t3/code", "rootPath": "/b/code"});
    let projects = [
        project("p1", "code", "/a/code", Some(identity)),
        project("p2", "code copy", "/b/code/", Some(other)),
        project("p3", "notes", "/notes", None),
    ];
    let groups = build_project_groups("env", &projects, &GroupingSettings::default());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].key, "github.com/t3/code");
    assert_eq!(groups[0].member_project_ids, ["p1", "p2"]);
    assert_eq!(groups[0].label, "t3/code");
    assert_eq!(groups[1].key, "env:/notes");
    let separate = GroupingSettings { mode: GroupingMode::Separate, ..Default::default() };
    assert_eq!(build_project_groups("env", &projects, &separate).len(), 3);
    let overrides = GroupingSettings { overrides: [("env:/b/code".to_owned(), GroupingMode::Separate)].into(), ..Default::default() };
    assert_eq!(build_project_groups("env", &projects, &overrides).len(), 3);
}

#[test]
fn repository_path_mode_keeps_monorepo_packages_apart() {
    let identity = |root: &str| json!({"canonicalKey": "github.com/t3/mono", "locator": {}, "rootPath": root});
    let projects = [project("web", "web", "/mono/apps/web", Some(identity("/mono"))), project("root", "mono", "/mono", Some(identity("/mono")))];
    let settings = GroupingSettings { mode: GroupingMode::RepositoryPath, ..Default::default() };
    let keys: Vec<String> = build_project_groups("env", &projects, &settings).into_iter().map(|g| g.key).collect();
    assert_eq!(keys, ["github.com/t3/mono::apps/web", "github.com/t3/mono"]);
    assert_eq!(normalize_project_path("C:/Users/X/"), "c:\\users\\x");
}

#[test]
fn older_settled_listing_and_counts() {
    let day = DAY_MS;
    let frontier = Cursor { settled_at: 50 * day, thread_id: None };
    assert!(is_listed_settled("recent", Some(60 * day), Some(&frontier)));
    assert!(!is_listed_settled("old", Some(30 * day), Some(&frontier)));
    assert!(is_listed_settled("old", Some(30 * day), None));
    let tie = Cursor { settled_at: 30 * day, thread_id: Some("m".into()) };
    assert!(is_listed_settled("a", Some(30 * day), Some(&tie)));
    assert!(!is_listed_settled("z", Some(30 * day), Some(&tie)));

    let older = OlderSettled { before: format_iso(50 * day), before_ms: 50 * day, counts: [("p1".to_owned(), 3)].into() };
    let held: Vec<ThreadShell> = [("snoozed-old", 10), ("paged-old", 20), ("recent", 60)]
        .iter()
        .map(|(id, settled_day)| settled_thread(id, &format_iso(settled_day * day)))
        .collect();
    let refs: Vec<&ThreadShell> = held.iter().collect();
    assert_eq!(unloaded_count(&older, None, &refs), 1);
    let other: HashSet<String> = ["other".to_owned()].into();
    assert_eq!(unloaded_count(&older, Some(&other), &refs), 0);
    assert_eq!(page_request(&tie, None)["before"], json!({"settledAt": format_iso(30 * day), "threadId": "m"}));

    // Search reaches threads below the cut: titles from two letters, messages from a long word.
    let before = older.before.as_str();
    assert_eq!(search_request("ab", None, None), None);
    assert_eq!(search_request(" ab ", Some(before), None), Some(json!({"query": "ab", "olderSettledBefore": before, "titlesOnly": true})));
    let scope = ["p1".to_owned()];
    assert_eq!(
        search_request("deploy", Some(before), Some(&scope)),
        Some(json!({"query": "deploy", "olderSettledBefore": before, "projectIds": ["p1"]}))
    );
    assert_eq!(search_request("deploy", None, Some(&scope)), Some(json!({"query": "deploy"})));
    let read: FamilyRead = serde_json::from_value(json!({"matches": []})).unwrap();
    assert!(read.threads.is_empty(), "a result without older rows mounts nothing");

    // Older threads the shell left out keep the Settled shelf once every loaded row is gone.
    let empty = snapshot(vec![]);
    let model = SidebarModel::build(
        &ModelInput {
            config_loaded: true,
            snapshot: &empty,
            extra_threads: &[],
            older_settled: Some(OlderSettledShelf { older: &older, frontier: None }),
            settings: &SidebarSettings::default(),
            shelves: &ShelfState::default(),
            selected_thread_id: None,
            project_scope: None, hidden_thread_ids: None,
            now: iso(NOW),
            dragging: None,
        },
        &mut InboxReturnTracker::default(),
    );
    assert_eq!(labels(&model), ["[Settled 3]"]);
}

fn wire_thread(id: &str, extra: Value) -> Value {
    let mut value = json!({
        "id": id, "projectId": "p1", "title": id, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": null, "activeRunId": null, "status": "idle", "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 0, "visibleItemCount": 0, "createdAt": "2026-04-01T00:00:00.000Z",
        "updatedAt": "2026-04-01T00:00:00.000Z", "archivedAt": null, "settledOverride": null,
        "settledAt": null, "deletedAt": null
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    value
}

fn settled_thread(id: &str, settled_at: &str) -> ThreadShell {
    serde_json::from_value(wire_thread(id, json!({"settledOverride": "settled", "settledAt": settled_at}))).unwrap()
}

fn snapshot(threads: Vec<Value>) -> ShellSnapshot {
    serde_json::from_value(json!({
        "snapshotSequence": 1,
        "projects": [{"id": "p1", "title": "T3", "workspaceRoot": "/w", "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z"}],
        "threads": threads,
    }))
    .unwrap()
}

fn build(snapshot: &ShellSnapshot, shelves: &ShelfState, selected: Option<&str>) -> SidebarModel {
    SidebarModel::build(
        &ModelInput { config_loaded: true, snapshot, extra_threads: &[], older_settled: None, settings: &SidebarSettings::default(), shelves, selected_thread_id: selected, project_scope: None, hidden_thread_ids: None, now: iso(NOW), dragging: None },
        &mut InboxReturnTracker::default(),
    )
}

fn labels(model: &SidebarModel) -> Vec<String> {
    model
        .rows
        .iter()
        .map(|row| match row {
            SidebarRow::Thread { index, section } => format!("{:?}:{}", section, model.threads[*index].id),
            SidebarRow::Shelf { shelf, count, expanded } => format!("[{} {count}{}]", shelf.label(), if *expanded { " open" } else { "" }),
            SidebarRow::ShowMore { count } => format!("more {count}"),
            SidebarRow::Marker(marker) => format!("<{marker:?}>"),
        })
        .collect()
}

#[test]
fn model_lists_sections_with_collapsed_shelves_and_the_open_thread() {
    let snapshot = snapshot(vec![
        wire_thread("pinned", json!({"pinnedAt": "2026-04-02T00:00:00.000Z"})),
        wire_thread("active", json!({})),
        wire_thread("busy", json!({"latestRunId": "r", "activeRunId": "r", "status": "running"})),
        wire_thread("napping", json!({"snoozedUntil": FUTURE_WAKE, "snoozedAt": SNOOZED_AT})),
        wire_thread("done-1", json!({"settledOverride": "settled", "settledAt": "2026-04-03T00:00:00.000Z"})),
        wire_thread("done-2", json!({"settledOverride": "settled", "settledAt": "2026-04-04T00:00:00.000Z"})),
        wire_thread("child", json!({"lineage": {"parentThreadId": "active", "relationshipToParent": "subagent", "rootThreadId": "active"}})),
        wire_thread("gone", json!({"archivedAt": "2026-04-05T00:00:00.000Z"})),
    ]);
    let model = build(&snapshot, &ShelfState::default(), Some("done-1"));
    assert_eq!(labels(&model), ["Pinned:pinned", "Active:active", "[Working 1]", "[Snoozed 1]", "[Settled 2]", "Settled:done-1"]);
    assert_eq!(model.next_change_at, Some(iso(FUTURE_WAKE)));
    let open = ShelfState { working_expanded: true, snoozed_expanded: true, settled_expanded: true, settled_visible: 1 };
    let model = build(&snapshot, &open, None);
    assert_eq!(
        labels(&model),
        ["Pinned:pinned", "Active:active", "[Working 1 open]", "Working:busy", "[Snoozed 1 open]", "Snoozed:napping", "[Settled 2 open]", "Settled:done-2", "more 1"]
    );
    assert_eq!(model.adjacent_thread(Some("active"), true), Some("busy"));
    assert_eq!(model.adjacent_thread(None, false), Some("done-2"));
    assert_eq!(model.jump_target(0), Some("pinned"));
    assert_eq!(model.section_of("napping"), Some(Section::Snoozed));

    let mut snapshot = snapshot;
    snapshot.threads.push(serde_json::from_value(wire_thread("busy-newer", json!({
        "latestRunId": "r2", "activeRunId": "r2", "status": "running",
        "latestUserAuthoredMessageAt": "2026-04-10T11:00:00.000Z", "activeOrderKey": "b"
    }))).unwrap());
    snapshot.threads[2].latest_user_authored_message_at = Some(Some("2026-04-10T10:00:00.000Z".into()));
    snapshot.threads[2].active_order_key = Some("c".into());
    for index in [0, 3, 4] {
        snapshot.threads[index].latest_run_id = Some("r".into());
        snapshot.threads[index].status = "running".into();
    }
    for enabled in [true, false] {
        let settings = SidebarSettings { working_shelf_enabled: enabled, ..Default::default() };
        let mut tracker = InboxReturnTracker::default();
        let model = SidebarModel::build(&ModelInput {
            config_loaded: true,
            snapshot: &snapshot, extra_threads: &[], older_settled: None, settings: &settings, shelves: &open,
            selected_thread_id: None, project_scope: None, hidden_thread_ids: None,
            now: iso(NOW), dragging: None,
        }, &mut tracker);
        let working: Vec<_> = model.partition.working.iter().map(|i| model.threads[*i].id.as_str()).collect();
        let active: Vec<_> = model.partition.active.iter().map(|i| model.threads[*i].id.as_str()).collect();
        assert_eq!(working, if enabled { vec!["busy-newer", "busy"] } else { vec![] }, "enabled={enabled}");
        assert_eq!(active, if enabled { vec!["active"] } else { vec!["active", "busy-newer", "busy"] });
        for (id, section) in [("pinned", Section::Pinned), ("napping", Section::Snoozed), ("done-1", Section::Settled)] {
            assert_eq!(model.section_of(id), Some(section));
        }
    }

    let mut completed = snapshot.clone();
    let newer = completed.threads.last_mut().unwrap();
    newer.status = "completed".into();
    newer.active_run_id = None;
    newer.latest_run_completed_at = Some(Some(NOW.into()));
    for kind in ["approval", "user_input"] {
        let mut tracker = InboxReturnTracker::default();
        let mut input = ModelInput {
            config_loaded: true,
            snapshot: &snapshot, extra_threads: &[], older_settled: None, settings: &SidebarSettings::default(), shelves: &open,
            selected_thread_id: None, project_scope: None, hidden_thread_ids: None, now: iso(NOW), dragging: None,
        };
        assert_eq!(SidebarModel::build(&input, &mut tracker).section_of("busy-newer"), Some(Section::Working));
        input.snapshot = &completed;
        for (elapsed, section) in [(1, Section::Working), (2_000, Section::Working), (2_001, Section::Active)] {
            input.now = iso(NOW) + elapsed;
            let model = SidebarModel::build(&input, &mut tracker);
            assert_eq!(model.section_of("busy-newer"), Some(section));
            if section == Section::Active {
                let active: Vec<_> = model.partition.active.iter().map(|i| model.threads[*i].id.as_str()).collect();
                assert_eq!(active, ["busy-newer", "active"]);
            }
        }
        input.snapshot = &snapshot;
        input.now += 1;
        assert_eq!(SidebarModel::build(&input, &mut tracker).section_of("busy-newer"), Some(Section::Working));
        input.snapshot = &completed;
        input.now += 1;
        assert_eq!(SidebarModel::build(&input, &mut tracker).section_of("busy-newer"), Some(Section::Working));
        let mut requested = completed.clone();
        requested.threads.last_mut().unwrap().pending_runtime_request = Some(crate::shell::PendingRuntimeRequestSummary {
            id: "request".into(), kind: kind.into(), created_at: None, blocking: Some(true),
        });
        input.snapshot = &requested;
        input.now += 1;
        assert_eq!(SidebarModel::build(&input, &mut tracker).section_of("busy-newer"), Some(Section::Active));
        assert_eq!(tracker.next_deadline(), None);
        input.snapshot = &snapshot;
        assert_eq!(SidebarModel::build(&input, &mut tracker).section_of("busy-newer"), Some(Section::Working));
    }
}

#[test]
fn dragging_adds_drop_markers_and_slots_resolve_through_them() {
    let snapshot = snapshot(vec![
        wire_thread("pinned", json!({"pinnedAt": "2026-04-02T00:00:00.000Z"})),
        wire_thread("active", json!({})),
        wire_thread("done-1", json!({"settledOverride": "settled", "settledAt": "2026-04-03T00:00:00.000Z"})),
    ]);
    let model = SidebarModel::build(
        &ModelInput { config_loaded: true, snapshot: &snapshot, extra_threads: &[], older_settled: None, settings: &SidebarSettings::default(), shelves: &ShelfState::default(), selected_thread_id: None, project_scope: None, hidden_thread_ids: None, now: iso(NOW), dragging: Some(Section::Active) },
        &mut InboxReturnTracker::default(),
    );
    assert_eq!(labels(&model), ["<PinnedHeader>", "Pinned:pinned", "<PinnedDivider>", "Active:active", "[Settled 1]", "<SettledPlaceholder>"]);
    let items = model.drop_items();
    let active = super::drag::position_of(&items, "active").unwrap();
    let settle_slot = model.drop_slot(5, false).unwrap();
    assert_eq!(super::drag::resolve_drop_target_at_slot(&items, active, settle_slot).map(|target| target.section), Some(Section::Settled));
    let pin_slot = model.drop_slot(1, false).unwrap();
    assert_eq!(super::drag::resolve_drop_target_at_slot(&items, active, pin_slot).map(|target| target.pinned_order), Some(vec!["active".to_owned(), "pinned".to_owned()]));
}

#[test]
fn subagent_approvals_flag_their_listed_parent() {
    let snapshot = snapshot(vec![
        wire_thread("parent", json!({})),
        wire_thread(
            "child",
            json!({"lineage": {"parentThreadId": "parent", "relationshipToParent": "subagent", "rootThreadId": "parent"},
                   "status": "running", "pendingRuntimeRequest": {"id": "q", "kind": "command_approval"}}),
        ),
    ]);
    let model = build(&snapshot, &ShelfState::default(), None);
    let parent = model.thread("parent").unwrap();
    assert_eq!(parent.pending_subagent_request, Some(SubagentRequest::Approval));
    assert_eq!(resolve_status(parent), ThreadStatus::Approval);
    assert!(model.thread("child").is_none());
}

#[test]
fn empty_shell_has_no_rows() {
    assert!(build(&snapshot(vec![]), &ShelfState::default(), None).rows.is_empty());
}

#[test]
fn splice_range_finds_the_smallest_change() {
    assert_eq!(splice_range(&[1, 2, 3], &[1, 2, 3]), None);
    assert_eq!(splice_range(&[1, 2, 3], &[1, 9, 3]), Some((1..2, 1)));
    assert_eq!(splice_range(&[1, 2, 3], &[1, 2, 3, 4]), Some((3..3, 1)));
    assert_eq!(splice_range(&[1, 2, 3], &[3]), Some((0..2, 0)));
    assert_eq!(splice_range::<i32>(&[], &[1]), Some((0..0, 1)));
}

#[test]
fn search_ranks_title_matches_before_branch_matches() {
    let snapshot = snapshot(vec![
        wire_thread("a", json!({"title": "Polish the sidebar", "branch": "feature/menu"})),
        wire_thread("b", json!({"title": "Fix menu bug"})),
        wire_thread("c", json!({"title": "Unrelated"})),
    ]);
    let model = build(&snapshot, &ShelfState::default(), None);
    assert_eq!(search_titles(&model, "menu", iso(NOW)), ["b", "a"]);
    assert!(search_titles(&model, "  ", iso(NOW)).is_empty());
}

/// America/Los_Angeles around the 2027-03-14 spring-forward.
struct LosAngeles;

impl Zone for LosAngeles {
    fn offset_seconds_at(&self, utc_ms: Ms) -> i64 {
        if utc_ms >= iso("2027-03-14T10:00:00.000Z") { -7 * 3600 } else { -8 * 3600 }
    }
}

#[test]
fn custom_snooze_rejects_invalid_or_past_calendar_input() {
    let now = local(2026, 9, 14, 14, 30);
    for (date, time) in [("2026-09-14", "14:30"), ("2027-02-29", "09:00"), ("2026-09-15", "24:00"), ("2026-9-15", "09:00")] {
        assert_eq!(resolve_custom_snooze(&CustomSnoozeInput::Date { date, time }, now, &FixedZone(0)), None, "{date} {time}");
    }
    assert_eq!(resolve_custom_snooze(&CustomSnoozeInput::Date { date: "2026-09-15", time: "09:00" }, now, &FixedZone(0)), Some(local(2026, 9, 15, 9, 0)));
    assert_eq!(local_snooze_fields(local(2026, 9, 15, 9, 5), &FixedZone(0)), ("2026-09-15".to_owned(), "09:05".to_owned()));
}

#[test]
fn custom_snooze_resolves_durations_from_the_confirmation_time() {
    let confirmed = local(2026, 9, 14, 14, 35);
    for (unit, amount, elapsed) in [(DurationUnit::Minutes, "0.5", 30_000), (DurationUnit::Minutes, "45", 45 * MINUTE_MS), (DurationUnit::Hours, "1.5", 90 * MINUTE_MS), (DurationUnit::Days, "2", 48 * HOUR_MS)] {
        assert_eq!(resolve_custom_snooze(&CustomSnoozeInput::Duration { amount, unit }, confirmed, &FixedZone(0)), Some(confirmed + elapsed));
    }
    assert_eq!(["0.00001", "0.000026666666666666667"].map(|amount| resolve_custom_snooze(&CustomSnoozeInput::Duration { amount, unit: DurationUnit::Minutes }, confirmed, &FixedZone(0))), [None, Some(confirmed + 1)]);
    for amount in ["0", "NaN", "1e300", "-1", ""] {
        assert_eq!(resolve_custom_snooze(&CustomSnoozeInput::Duration { amount, unit: DurationUnit::Hours }, confirmed, &FixedZone(0)), None, "{amount}");
    }
}

#[test]
fn custom_snooze_rejects_the_spring_gap_and_keeps_days_at_24_hours() {
    let input = CustomSnoozeInput::Date { date: "2027-03-14", time: "02:30" };
    assert_eq!(resolve_custom_snooze(&input, iso("2027-03-01T00:00:00Z"), &LosAngeles), None);
    let before = from_local(2027, 3, 13, 12, 0, &LosAngeles);
    let wake = resolve_custom_snooze(&CustomSnoozeInput::Duration { amount: "1", unit: DurationUnit::Days }, before, &LosAngeles);
    assert_eq!(wake, Some(from_local(2027, 3, 14, 13, 0, &LosAngeles)));
}

#[test]
fn id_ties_follow_locale_compare() {
    let dash = SidebarThread { pinned: true, created_at: Some(1), ..base("a-b") };
    let underscore = SidebarThread { pinned: true, created_at: Some(1), ..base("a_b") };
    let mut list = vec![&dash, &underscore];
    sort_pinned(&mut list);
    assert_eq!(list.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a_b", "a-b"]);
}

#[test]
fn streaming_ticks_are_quiet_but_real_changes_are_not() {
    let running = json!({"latestRunId": "r", "activeRunId": "r", "status": "running", "itemCount": 3, "visibleItemCount": 2});
    let before: ThreadShell = serde_json::from_value(wire_thread("t", running.clone())).unwrap();
    let mut tick = before.clone();
    tick.updated_at = "2026-04-01T00:00:05.000Z".into();
    tick.item_count = 9;
    assert!(same_apart_from_streaming(&before, &tick));
    let mut renamed = tick.clone();
    renamed.title = "Renamed".into();
    assert!(!same_apart_from_streaming(&before, &renamed));
    let idle: ThreadShell = serde_json::from_value(wire_thread("t", json!({"latestRunId": "r", "status": "completed"}))).unwrap();
    let mut idle_tick = idle.clone();
    idle_tick.updated_at = "2026-04-01T00:00:05.000Z".into();
    assert!(!same_apart_from_streaming(&idle, &idle_tick));
}

#[test]
fn tag_visibility_applies_before_sidebar_partition_and_search() {
    let snapshot = snapshot(vec![wire_thread("private",json!({"tags":["private"]})),wire_thread("plain",json!({})),
        wire_thread("delegated",json!({"delegatedFrom":{"environmentId":"env","threadId":"private","title":"Private"}}))]);
    let refs: Vec<_> = snapshot.threads.iter().collect();
    let tags = vec!["private".into()];
    let hidden = tag_filter::hidden_thread_keys(&[tag_filter::Environment{environment_id:"env",threads:&refs,hidden_tags:Some(&tags)}],&HashSet::new(),&HashSet::new(),true)
        .into_iter().map(|key|key.thread_id).collect();
    let model = SidebarModel::build(&ModelInput {config_loaded:true,snapshot:&snapshot,extra_threads:&[],older_settled:None,settings:&Default::default(),shelves:&Default::default(),selected_thread_id:Some("private"),project_scope:None,hidden_thread_ids:Some(&hidden),now:iso(NOW),dragging:None},&mut Default::default());
    assert_eq!(model.threads.iter().map(|thread|thread.id.as_str()).collect::<Vec<_>>(),["plain"]);
    assert!(search_titles(&model,"private",iso(NOW)).is_empty());
    assert_eq!(model.jump_target(0),Some("plain"));
}

#[test]
fn deletion_fallback_stays_in_project_and_skips_parked_threads() {
    let now = iso(NOW);
    let future = iso(FUTURE_WAKE);
    let snoozed_at = iso(SNOOZED_AT);
    let napping = SidebarThread {
        snoozed_until: Some(future),
        snoozed_at: Some(snoozed_at),
        ..base("parked")
    };
    let failed = SidebarThread {
        latest_run: Some(RunSummary {
            completed_at: Some(snoozed_at + 1),
            ..run("failed", None, None)
        }),
        runtime: Some(runtime("failed")),
        ..napping.clone()
    };
    for (candidate, eligible) in [
        (
            SidebarThread {
                project_id: "another-physical-project".into(),
                ..base("parked")
            },
            false,
        ),
        (
            SidebarThread {
                settled_override: Some(SettledOverride::Settled),
                pinned: true,
                has_pending_approvals: true,
                ..base("parked")
            },
            false,
        ),
        (napping.clone(), false),
        (
            SidebarThread {
                snoozed_until: Some(now),
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                snoozed_until: Some(now - 1),
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                has_pending_approvals: true,
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                has_pending_user_input: true,
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                has_pending_runtime_request: true,
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                pending_subagent_request: Some(SubagentRequest::Approval),
                ..napping.clone()
            },
            true,
        ),
        (
            SidebarThread {
                pending_guest_message_count: 1,
                ..napping.clone()
            },
            true,
        ),
        (failed.clone(), true),
        (
            SidebarThread {
                latest_run: Some(run("failed", None, None)),
                ..failed.clone()
            },
            false,
        ),
        (
            SidebarThread {
                latest_run: Some(RunSummary {
                    completed_at: Some(snoozed_at),
                    ..run("failed", None, None)
                }),
                ..failed
            },
            false,
        ),
        (
            SidebarThread {
                settled_override: Some(SettledOverride::Active),
                ..base("parked")
            },
            true,
        ),
        (
            SidebarThread {
                pinned: true,
                ..base("parked")
            },
            true,
        ),
        (
            SidebarThread {
                pinned: true,
                settled_override: Some(SettledOverride::Active),
                ..napping
            },
            false,
        ),
    ] {
        let threads = vec![candidate, base("deleted")];
        let select = |at| {
            fallback_after_delete(
                &threads,
                "deleted",
                &HashSet::new(),
                ThreadSortOrder::UpdatedAt,
                at,
            )
        };
        assert_eq!(
            select(now),
            eligible.then_some("parked"),
            "{:?}",
            threads[0]
        );
        // The instant comes from deletion, not the last sidebar rebuild.
        if threads[0].project_id == "p1" && !threads[0].is_settled() {
            assert_eq!(select(future), Some("parked"));
        }
    }
}

#[test]
fn deletion_fallback_uses_current_sort_and_excludes_the_entire_batch() {
    use crate::client_settings::ClientSettingsDoc;
    let threads = vec![
        SidebarThread {
            created_at: Some(10),
            updated_at: Some(20),
            latest_user_message_at: Some(50),
            ..base("user-message")
        },
        SidebarThread {
            created_at: Some(30),
            updated_at: Some(40),
            ..base("tie-a")
        },
        SidebarThread {
            created_at: Some(30),
            updated_at: Some(40),
            ..base("tie-z")
        },
        base("deleted"),
    ];
    for (json, expected) in [
        ("{}", "user-message"),
        (r#"{"sidebarThreadSortOrder":"updated_at"}"#, "user-message"),
        (r#"{"sidebarThreadSortOrder":"created_at"}"#, "tie-z"),
        (r#"{"sidebarThreadSortOrder":"invalid"}"#, "user-message"),
        (r#"{"sidebarThreadSortOrder":false}"#, "user-message"),
    ] {
        let settings = ClientSettingsDoc::from_json(json);
        let before = settings.to_json();
        assert_eq!(
            fallback_after_delete(
                &threads,
                "deleted",
                &HashSet::new(),
                settings.thread_sort_order(),
                100
            ),
            Some(expected)
        );
        assert_eq!(settings.to_json(), before);
    }
    let select = |id, doomed: &HashSet<String>| {
        fallback_after_delete(&threads, id, doomed, ThreadSortOrder::CreatedAt, 100)
    };
    assert_eq!(select("missing", &HashSet::new()), None);
    assert_eq!(
        select("deleted", &["deleted".into(), "tie-z".into()].into()),
        Some("tie-a")
    );
    assert_eq!(
        select("deleted", &threads.iter().map(|t| t.id.clone()).collect()),
        None
    );
    assert_eq!(
        fallback_after_delete(
            &[],
            "deleted",
            &HashSet::new(),
            ThreadSortOrder::UpdatedAt,
            100
        ),
        None
    );
    let undated = [
        SidebarThread {
            created_at: None,
            updated_at: None,
            ..base("z")
        },
        SidebarThread {
            created_at: None,
            updated_at: None,
            ..base("a")
        },
        base("deleted"),
    ];
    assert_eq!(
        fallback_after_delete(
            &undated,
            "deleted",
            &HashSet::new(),
            ThreadSortOrder::UpdatedAt,
            100
        ),
        Some("z")
    );
}


fn scoped_fixture(saved: &str, canonical: Option<&str>, path: &str, scratch: bool) -> (ScopedProject, super::fleet::ScopedThread) {
    let owner = EnvironmentOwner { saved_id: saved.into(), canonical_id: canonical.map(str::to_owned), generation: 1, connection_generation: 1, shell_generation: 1 };
    let project = ScopedProject { owner: owner.clone(), project: project("p1", saved, path, Some(json!({"canonicalKey":"repo"}))), is_scratch: scratch, environment_label: saved.into() };
    let thread = super::fleet::ScopedThread { owner, thread: base("same"), created_at: "2026-06-01T00:00:00.000Z".into(), lineage: crate::shell::Lineage { parent_thread_id: None, relationship_to_parent: None, root_thread_id: "same".into(), side_chat: None }, delegated_from: None, tags: Vec::new() };
    (project, thread)
}

#[test]
fn scoped_groups_keep_colliding_refs_physical_winners_and_saved_aliases() {
    let (a, ta) = scoped_fixture("a", Some("server-a"), "/repo", false);
    let (b, mut tb) = scoped_fixture("b", Some("server-b"), "/repo", false);
    tb.thread.latest_user_message_at = Some(iso("2026-07-01T00:00:00Z"));
    let mut alias = a.clone();
    alias.owner.saved_id = "alias".into();
    let mut newer = a.clone();
    newer.project.id = "p-new".into();
    newer.project.updated_at = "2026-02-01T00:00:00Z".into();
    newer.project.repository_identity = None;
    let projects = [a, b, alias, newer];
    let threads = [ta, tb];
    let settings = GroupingSettings::default();
    let preferences = ProjectPreferences::default();
    let input = GroupInput { projects: &projects, threads: &threads, settings: &settings, preferences: &preferences, preferred_environment: Some("b"), consumer: GroupConsumer::Palette, sort: ProjectSortOrder::UpdatedAt };
    let groups = build_scoped_project_groups(&input);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].members.len(), 2);
    assert_eq!(groups[0].member_refs.len(), 4);
    assert_eq!(groups[0].mutation_refs().len(), 3);
    let aliased = build_scoped_project_groups(&GroupInput { projects: &[projects[0].clone(), projects[2].clone()], preferred_environment: Some("alias"), ..input });
    assert_eq!(aliased[0].representative.environment_id, "alias");
    assert_eq!(aliased[0].mutation_refs(), [projects[2].reference()]);
    assert_eq!(pick_project_target(&aliased[0], Some(&projects[2].reference())).unwrap().project.owner.saved_id, "alias");
    assert_eq!(groups[0].representative.environment_id, "b");
    assert_eq!(pick_project_target(&groups[0], Some(&projects[2].reference())).unwrap().project.project.id, projects[2].project.id);
    assert_eq!(pick_project_target(&groups[0], Some(&projects[0].reference())).unwrap().project.project.id, "p-new");
    assert_eq!(latest_group_thread(&groups[0], &threads, ThreadSortOrder::UpdatedAt), Some(tag_filter::ThreadKey::new("b", "same")));
    assert_eq!(latest_group_thread(&groups[0], &threads, ThreadSortOrder::CreatedAt), Some(tag_filter::ThreadKey::new("a", "same")));
    let unknown = [scoped_fixture("a", None, "/repo", false).0, scoped_fixture("b", None, "/repo", false).0];
    let settings = GroupingSettings { mode: GroupingMode::Separate, ..Default::default() };
    let separated = build_scoped_project_groups(&GroupInput { projects: &unknown, settings: &settings, ..input });
    assert_eq!(separated.len(), 2);
    assert_ne!(separated[0].key, separated[1].key);
}

#[test]
fn scoped_preferences_keep_missing_intent_scratch_migration_and_consumer_order() {
    let (a, _) = scoped_fixture("a", Some("server-a"), "/scratch", true);
    let (b, _) = scoped_fixture("b", Some("server-b"), "/scratch", true);
    let mut ordinary = a.clone(); ordinary.project.id = "ordinary".into(); ordinary.project.workspace_root = "/ordinary".into(); ordinary.project.title = "AAA".into(); ordinary.is_scratch = false; ordinary.project.repository_identity = None;
    let projects = [a, b, ordinary];
    let settings = GroupingSettings { mode: GroupingMode::Separate, ..Default::default() };
    let mut preferences = ProjectPreferences { scope_keys: vec![projects[0].physical_key()], project_order: vec![legacy_project_cwd_key("/ordinary")], ..Default::default() };
    let input = GroupInput { projects: &projects, threads: &[], settings: &settings, preferences: &preferences, preferred_environment: None, consumer: GroupConsumer::Sidebar, sort: ProjectSortOrder::Manual };
    let groups = build_scoped_project_groups(&input);
    assert_eq!(groups[0].representative.project_id, "ordinary");
    let scratch = groups.iter().find(|group| group.is_scratch).unwrap();
    assert_eq!(scratch.key, SCRATCH_SCOPE_KEY);
    assert_eq!(preferences.resolved_scope(&groups, true), [SCRATCH_SCOPE_KEY]);
    assert_eq!(preferences.scoped_members(&groups, true).unwrap().len(), 2);
    assert_eq!(preferences.scoped_members(&[], true), None);
    assert_eq!(preferences.scope_keys, [projects[0].physical_key()]);
    let settings_groups = build_scoped_project_groups(&GroupInput { consumer: GroupConsumer::Settings, ..input });
    assert_eq!(settings_groups.len(), 3);
    assert_eq!(settings_groups.iter().map(|group| group.label.as_str()).collect::<Vec<_>>(), ["a", "AAA", "b"]);
    let mut pinned_projects = projects.clone();
    pinned_projects[1].project.pinned_at = Some("2026-01-01T00:00:00Z".into());
    let pinned_groups = build_scoped_project_groups(&GroupInput { projects:&pinned_projects, ..input });
    assert!(pinned_groups[0].pinned, "Sidebar stably puts pinned logical groups before the physical manual order");
    preferences.project_expanded.insert(legacy_project_cwd_key("/scratch"), false);
    assert!(!preferences.expanded(scratch));
    preferences.set_expanded(scratch, true);
    assert!(preferences.expanded(scratch));
    let unknown = scoped_fixture("a", None, "/scratch", true).0;
    let old_key = unknown.physical_key();
    let old = build_scoped_project_groups(&GroupInput { projects: &[unknown], settings: &settings, preferences: &ProjectPreferences::default(), threads: &[], preferred_environment: None, consumer: GroupConsumer::Settings, sort: ProjectSortOrder::Manual });
    preferences.scope_keys = vec![old_key.clone()]; preferences.project_order = vec![old_key];
    preferences.reconcile(&old, &settings_groups);
    assert_eq!(preferences.scope_keys, [projects[0].physical_key()]);
    assert_eq!(preferences.project_order, [projects[0].physical_key()]);
    preferences.reorder(&["a".into(),"b".into(),"c".into()], &["a".into()], &["b".into()]);
    assert_eq!(preferences.project_order, ["b","a","c"]);
}

#[test]
fn scoped_sidebar_collisions_folder_selected_route_and_per_owner_frontier() {
    use super::fleet::*;
    let (a, ta) = scoped_fixture("a", Some("server-a"), "/a", false);
    let (b, tb) = scoped_fixture("b", Some("server-b"), "/b", false);
    let projects = [a.clone(), b.clone()];
    let threads = [ta.clone(), tb.clone()];
    let settings = GroupingSettings::default();
    let mut preferences = ProjectPreferences::default();
    let groups = build_scoped_project_groups(&GroupInput { projects: &projects, threads: &threads, settings: &settings, preferences: &preferences, preferred_environment: None, consumer: GroupConsumer::Sidebar, sort: ProjectSortOrder::UpdatedAt });
    preferences.set_expanded(&groups[0], false);
    let ea = EnvironmentSummary { owner: a.owner.clone(), projects: vec![a], threads: vec![ta], synchronized: true, config_loaded: true, snapshot_sequence: Some(1), older_settled: None, removed_at: HashMap::new(), older_count_key: None };
    let eb = EnvironmentSummary { owner: b.owner.clone(), projects: vec![b], threads: vec![tb], synchronized: true, config_loaded: true, snapshot_sequence: Some(1), older_settled: None, removed_at: HashMap::new(), older_count_key: None };
    let selected = ThreadKey::new("b", "same");
    let sidebar = SidebarSettings::default(); let shelves = ShelfState::default(); let hidden = HashSet::new();
    let mut state = ScopedSidebarState::default();
    let environments = [&ea, &eb];
    let input = ScopedSidebarInput { environments: &environments, groups: &groups, preferences: &preferences, settings: &sidebar, shelves: &shelves, selected: Some(&selected), hidden: &hidden, older: &[], extra_threads: &[], now: 0, folder_mode: true, dragging: None };
    let model = ScopedSidebarModel::build(&input, &mut state);
    assert_eq!(model.index_by_key.len(), 2);
    assert_eq!(model.ordered.len(), 1);
    assert_eq!(model.jump_target(0), Some(selected.clone()));
    assert!(model.rows.iter().any(|row| matches!(row, ScopedSidebarRow::Folder { hidden_count: 1, expanded: false, .. })));
    let normal = ScopedSidebarModel::build(&ScopedSidebarInput { folder_mode: false, ..input }, &mut state);
    assert_eq!(normal.ordered.len(), 2);
    assert_ne!(normal.row_keys()[0], normal.row_keys()[1]);
    let mut alias = ea.clone(); alias.owner.saved_id = "alias".into();
    for thread in &mut alias.threads { thread.owner = alias.owner.clone(); }
    let aliases = [&ea, &eb, &alias];
    let deduped = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &aliases, folder_mode: false, ..input }, &mut state);
    assert_eq!(deduped.threads.len(), 2);
    assert!(deduped.thread(&ThreadKey::new("a", "same")).is_some());
    let hidden_alias = [ThreadKey::new("a", "same")].into();
    let eligible = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &aliases, hidden: &hidden_alias, folder_mode: false, ..input }, &mut state);
    assert_eq!(eligible.threads.len(), 2);
    assert!(eligible.thread(&ThreadKey::new("alias", "same")).is_some());
    let mut empty_a = ea.clone(); empty_a.threads.clear();
    let mut empty_alias = alias.clone(); empty_alias.threads.clear();
    let extra_a = [ea.threads[0].clone()]; let extra_alias = [alias.threads[0].clone()];
    let extra = [ScopedExtraThreads { owner: &ea.owner, threads: &extra_a }, ScopedExtraThreads { owner: &alias.owner, threads: &extra_alias }];
    let empty_owners = [&empty_a, &empty_alias];
    let omitted_aliases = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &empty_owners, extra_threads: &extra, folder_mode: false, selected: None, ..input }, &mut state);
    assert_eq!(omitted_aliases.threads.len(), 1);
    let mut ea = ea.clone(); let mut eb = eb.clone();
    for environment in [&mut ea, &mut eb] { environment.threads[0].thread.settled_override = Some(SettledOverride::Settled); environment.threads[0].thread.settled_at = Some(10); }
    let older = OlderSettled { before: format_iso(20), before_ms: 20, counts: [("p1".into(), 2)].into() };
    let frontier = Cursor { settled_at: 15, thread_id: None };
    let owners = [ScopedOlderSettled { owner: &ea.owner, older: &older, frontier: Some(&frontier) }, ScopedOlderSettled { owner: &eb.owner, older: &older, frontier: None }];
    let environments = [&ea, &eb];
    let model = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &environments, older: &owners, folder_mode: false, selected: None, ..input }, &mut state);
    assert_eq!(model.settled_count, 4);
    assert_eq!(model.listed_settled_count, 0);
    let at_time = Cursor { settled_at: 10, thread_id: None };
    let low_id = Cursor { settled_at: 10, thread_id: Some("a".into()) };
    let high_id = Cursor { settled_at: 10, thread_id: Some("z".into()) };
    for (left, right, listed) in [(&at_time, &high_id, 2), (&high_id, &low_id, 0)] {
        let owners = [ScopedOlderSettled { owner: &ea.owner, older: &older, frontier: Some(left) }, ScopedOlderSettled { owner: &eb.owner, older: &older, frontier: Some(right) }];
        let model = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &environments, older: &owners, folder_mode: false, selected: None, ..input }, &mut state);
        assert_eq!(model.listed_settled_count, listed);
    }
    let mut failed = super::older_settled::SettledPages::default();
    failed.sync_generation(ea.owner.shell_generation, Some(&older));
    failed.status = super::older_settled::PageStatus::Failed;
    let owners = [ScopedOlderSettled { owner: &ea.owner, older: &older, frontier: failed.listing_frontier(Some(&older)) }, ScopedOlderSettled { owner: &eb.owner, older: &older, frontier: None }];
    let model = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &environments, older: &owners, folder_mode: false, selected: None, ..input }, &mut state);
    assert_eq!(model.listed_settled_count, 2, "failed and exhausted owners do not hold back the shelf");
    let owners = [ScopedOlderSettled { owner: &ea.owner, older: &older, frontier: Some(&frontier) }, ScopedOlderSettled { owner: &eb.owner, older: &older, frontier: None }];
    let model = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &environments, older: &owners, folder_mode: false, selected: Some(&selected), ..input }, &mut state);
    assert_eq!(model.listed_settled_count, 0);
    assert_eq!(model.jump_target(0), Some(selected.clone()), "the selected row stays visible below the shared frontier");
    let mut alias = ea.clone(); alias.owner.saved_id = "alias".into();
    for thread in &mut alias.threads { thread.owner = alias.owner.clone(); }
    let aliases = [&ea, &alias];
    let older_aliases = [ScopedOlderSettled { owner: &ea.owner, older: &older, frontier: None }, ScopedOlderSettled { owner: &alias.owner, older: &older, frontier: None }];
    let model = ScopedSidebarModel::build(&ScopedSidebarInput { environments: &aliases, older: &older_aliases, folder_mode: false, selected: None, ..input }, &mut state);
    assert_eq!(model.settled_count, 2);

    let mut scope_groups = groups.clone();
    scope_groups[0].key = "chosen".into();
    let mut pinned_empty = scope_groups[0].clone();
    pinned_empty.key = "pinned-empty".into(); pinned_empty.member_refs.clear(); pinned_empty.pinned = true;
    let mut archived_empty = pinned_empty.clone();
    archived_empty.key = "archived-empty".into(); archived_empty.archived = true;
    let mut archived_active = scope_groups[0].clone();
    archived_active.key = "archived-active".into(); archived_active.archived = true;
    scope_groups.extend([pinned_empty, archived_empty]);
    let mut older_send = threads[0].clone();
    older_send.thread.id = "older-send".into(); older_send.thread.active_order_key = Some("a".into());
    older_send.thread.created_at = Some(1); older_send.thread.latest_user_authored_message_at = Some(Some(10));
    let mut newer_send = older_send.clone();
    newer_send.thread.id = "newer-send".into(); newer_send.thread.active_order_key = Some("z".into());
    newer_send.thread.latest_user_authored_message_at = Some(Some(20));
    let mut environment = ea.clone(); environment.threads = vec![older_send, newer_send];
    let environments = [&environment];
    let prefs = ProjectPreferences::default();
    let sidebar = SidebarSettings { working_shelf_enabled:false, ..Default::default() };
    let folder_input = ScopedSidebarInput { environments: &environments, groups: &scope_groups, preferences: &prefs, settings:&sidebar, selected: None, older: &[], folder_mode: true, ..input };
    let reports = [
        ("folder-send", true, Vec::new()),
        ("normal-manual", false, Vec::new()),
        ("scoped-only", true, vec!["chosen".into()]),
    ].map(|(name, folder_mode, scope_keys)| {
        let preferences = ProjectPreferences { scope_keys, ..Default::default() };
        let model = ScopedSidebarModel::build(&ScopedSidebarInput { preferences: &preferences, folder_mode, ..folder_input }, &mut ScopedSidebarState::default());
        let folders = model.rows.iter().filter_map(|row| match row { ScopedSidebarRow::Folder { key, .. } => Some(key.clone()), _ => None }).collect::<Vec<_>>();
        let order = model.ordered.iter().map(|index| model.threads[*index].thread.id.clone()).collect::<Vec<_>>();
        (name, folders, order)
    });
    assert_eq!(reports, [
        ("folder-send", vec!["chosen".into(), "pinned-empty".into()], vec!["newer-send".into(), "older-send".into()]),
        ("normal-manual", vec![], vec!["older-send".into(), "newer-send".into()]),
        ("scoped-only", vec!["chosen".into()], vec!["newer-send".into(), "older-send".into()]),
    ]);
    let archived = [archived_active];
    let model = ScopedSidebarModel::build(&ScopedSidebarInput { groups: &archived, ..folder_input }, &mut ScopedSidebarState::default());
    assert!(model.rows.iter().any(|row| matches!(row, ScopedSidebarRow::Folder { archived:true, .. })));
    assert_eq!(model.ordered.len(), 2, "archived projects keep folders only for their Active rows");
}


#[test]
fn scoped_sections_follow_loaded_config_per_owner_and_keep_cached_disconnects() {
    use super::fleet::*;
    use crate::{server_config::ServerConfigState, shell::ShellState};

    let mut shell = ShellState::default();
    shell.snapshot = Some(snapshot(vec![
        wire_thread("snoozed", json!({"pinnedAt":SNOOZED_AT,"settledOverride":"settled","settledAt":SNOOZED_AT,"snoozedAt":SNOOZED_AT,"snoozedUntil":FUTURE_WAKE})),
        wire_thread("settled", json!({"pinnedAt":SNOOZED_AT,"settledOverride":"settled","settledAt":SNOOZED_AT})),
        wire_thread("working", json!({"status":"running","latestRunId":"run","activeRunId":"run"})),
    ]));
    shell.synchronized = true;
    let absent = ServerConfigState::default();
    let mut loaded = ServerConfigState::default();
    loaded.apply(serde_json::from_value(json!({"type":"snapshot","config":{}})).unwrap());
    let owner = |saved: &str| EnvironmentOwner {
        saved_id:saved.into(), canonical_id:Some(format!("canonical-{saved}")),
        generation:1, connection_generation:1, shell_generation:shell.generation,
    };
    let pending = EnvironmentSummary::from_shell(owner("pending"), &shell, &absent);
    let ready = EnvironmentSummary::from_shell(owner("ready"), &shell, &loaded);
    let settings = SidebarSettings::default();
    let preferences = ProjectPreferences::default();
    let shelves = ShelfState::default();
    let hidden = HashSet::new();
    for config in [&absent,&loaded] {
        let model = SidebarModel::build(&ModelInput {
            snapshot:shell.snapshot.as_ref().unwrap(), config_loaded:config.config.is_some(), extra_threads:&[], older_settled:None,
            settings:&settings, shelves:&shelves, selected_thread_id:None, project_scope:None, hidden_thread_ids:None, now:iso(NOW), dragging:None,
        }, &mut InboxReturnTracker::default());
        assert_eq!(model.section_of("snoozed"),Some(if config.config.is_some() { Section::Snoozed } else { Section::Pinned }));
        assert_eq!(model.section_of("settled"),Some(if config.config.is_some() { Section::Settled } else { Section::Pinned }));
        assert_eq!(model.section_of("working"),Some(Section::Working));
    }
    let mut model_shell = shell.clone();
    let mut changed_thread = shell.thread("working").unwrap().clone();
    changed_thread.model_selection.model = "destination-model-v2".into();
    assert!(model_shell.apply(serde_json::from_value(json!({
        "kind":"thread.updated", "sequence":2, "location":"active", "thread":changed_thread,
    })).unwrap()));
    let model_changed = EnvironmentSummary::from_shell(owner("ready"), &model_shell, &loaded);
    let before_thread = ready.threads.iter().find(|thread| thread.thread.id == "working").unwrap();
    let changed_thread = model_changed.threads.iter().find(|thread| thread.thread.id == "working").unwrap();
    assert_eq!(before_thread.owner, changed_thread.owner);
    assert_ne!(before_thread.thread, changed_thread.thread, "a destination model-only change must reach shared sidebar metadata");
    for folder_mode in [false, true] {
        let model_for = |environment: &EnvironmentSummary| ScopedSidebarModel::build(&ScopedSidebarInput {
            environments:&[environment], groups:&[], preferences:&preferences, settings:&settings, shelves:&shelves,
            selected:None, hidden:&hidden, older:&[], extra_threads:&[], now:iso(NOW), folder_mode, dragging:None,
        }, &mut ScopedSidebarState::default());
        let before_model = model_for(&ready);
        let changed_model = model_for(&model_changed);
        let key = ThreadKey::new("ready", "working");
        assert_ne!(before_model.thread(&key).unwrap().thread, changed_model.thread(&key).unwrap().thread, "the scoped row consumer must retain destination model-only changes");
        let assert_sections = |environments: &[&EnvironmentSummary], pending_loaded: bool| {
            let model = ScopedSidebarModel::build(&ScopedSidebarInput {
                environments, groups:&[], preferences:&preferences, settings:&settings, shelves:&shelves,
                selected:None, hidden:&hidden, older:&[], extra_threads:&[], now:iso(NOW), folder_mode, dragging:None,
            }, &mut ScopedSidebarState::default());
            assert_eq!(model.threads.len(), 6, "same raw ids in distinct owners remain distinct");
            for (saved, config_loaded) in [("pending",pending_loaded),("ready",true)] {
                for (id, expected) in [
                    ("snoozed", if config_loaded { Section::Snoozed } else { Section::Pinned }),
                    ("settled", if config_loaded { Section::Settled } else { Section::Pinned }),
                    ("working", if folder_mode { Section::Active } else { Section::Working }),
                ] {
                    assert_eq!(model.section_of(&ThreadKey::new(saved,id)),Some(expected),"owner={saved} config_loaded={config_loaded} folder_mode={folder_mode} thread={id}");
                }
            }
        };
        assert_sections(&[&pending,&ready],false);
        let pending_loaded = EnvironmentSummary::from_shell(owner("pending"), &shell, &loaded);
        assert_sections(&[&pending_loaded,&ready],true);
        let mut disconnected = shell.clone();
        disconnected.synchronized = false;
        let cached_pending = EnvironmentSummary::from_shell(owner("pending"), &disconnected, &loaded);
        let cached_ready = EnvironmentSummary::from_shell(owner("ready"), &disconnected, &loaded);
        assert_sections(&[&cached_pending,&cached_ready],true);
    }
}

#[test]
fn scoped_quiet_metadata_keeps_existing_omitted_row_rules() {
    use crate::families::{MountedReads, FamilyRead};
    let row = serde_json::from_value::<ThreadShell>(json!({
        "id":"extra", "projectId":"p1", "title":"extra", "providerInstanceId":"replay", "modelSelection":{"instanceId":"replay","model":"replay"},
        "runtimeMode":"auto", "interactionMode":"default", "lineage":{"rootThreadId":"extra"}, "status":"idle", "hasActionableProposedPlan":false,
        "itemCount":0, "visibleItemCount":0, "createdAt":"2026-01-01T00:00:00Z", "updatedAt":"2026-01-01T00:00:00Z"
    })).unwrap();
    let mut mounts = MountedReads::default();
    mounts.mount("pages", 4, vec![FamilyRead { snapshot_sequence: 8, threads: vec![row.clone()] }]);
    assert_eq!(mounts.extras_for(4, 8, &HashSet::new(), &HashMap::new()).len(), 1);
    assert!(mounts.extras_for(5, 8, &HashSet::new(), &HashMap::new()).is_empty());
    assert!(mounts.extras_for(4, 9, &HashSet::new(), &HashMap::new()).is_empty());
    assert!(mounts.extras_for(4, 8, &["extra".into()].into(), &HashMap::new()).is_empty());
    assert!(mounts.extras_for(4, 8, &HashSet::new(), &[("extra".into(), 9)].into()).is_empty());
    let cut = crate::shell::OlderSettledCut { before: "2026-06-01T00:00:00Z".into(), as_of_sequence:8, counts:vec![crate::shell::OlderSettledCount { project_id:"p1".into(), count:2 }] };
    let mut counts = OlderSettledCounts::default();
    assert_eq!(counts.sync_snapshot(4, Some(8), Some(&cut), Some("snapshot")), None);
    assert_eq!(counts.current().unwrap().counts["p1"], 2);
    assert_eq!(counts.sync_snapshot(4, Some(9), Some(&cut), Some("changed")), Some("changed".into()));
    counts.apply_read("old", [("p1".into(), 7)].into());
    assert_eq!(counts.current().unwrap().counts["p1"], 2);
    counts.apply_read("changed", [("p1".into(), 1)].into());
    assert_eq!(counts.current().unwrap().counts["p1"], 1);
    let mut pages = SettledPages::default();
    pages.sync_generation(4, counts.current().as_ref());
    pages.grow(counts.current().as_ref(), 0, 1);
    let (generation, before) = pages.next_request().unwrap();
    pages.sync_generation(5, counts.current().as_ref());
    assert!(!pages.apply_page(generation, &before, Ok(FamilyRead { snapshot_sequence:9, threads:vec![row] })));
}
