use serde_json::{Value, json};

use super::*;

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).unwrap()
}

fn panel(value: Value) -> ProviderPanelSettings {
    settings(value).provider_panel
}

#[test]
fn updates_default_on_and_decode_alone() {
    assert_eq!(settings(json!({})).maintenance, MaintenanceSettings::default());
    let decoded = settings(json!({ "enableProviderUpdateChecks": false, "continueThreadsAfterServerUpdate": "no", "keepAwakeWhileAgentsWork": false })).maintenance;
    assert!(!decoded.enable_provider_update_checks);
    assert!(decoded.continue_threads_after_server_update, "a bad value takes the default");
    assert!(!decoded.keep_awake_while_agents_work);
}

#[test]
fn retention_reads_beside_the_worktree_rules() {
    let decoded = settings(json!({ "storageCleanup": { "browserArtifactsAfterDays": 8, "logsAfterDays": 0, "worktreeOnMerge": true } }));
    assert_eq!(Retention::of(&decoded), Retention { browser_artifacts_after_days: Some(8), logs_after_days: None });
    assert!(decoded.worktrees.storage_cleanup.rules.worktree_on_merge, "the worktree rules still decode");
}

#[test]
fn change_payloads_patch_one_key() {
    assert_eq!(MaintenanceChange::ProviderUpdateChecks(false).payload(), json!({ "patch": { "enableProviderUpdateChecks": false } }));
    assert_eq!(MaintenanceChange::ContinueThreads(true).payload(), json!({ "patch": { "continueThreadsAfterServerUpdate": true } }));
    assert_eq!(MaintenanceChange::KeepAwakeWhileAgentsWork(false).payload(), json!({ "patch": { "keepAwakeWhileAgentsWork": false } }));
    assert_eq!(MaintenanceChange::BrowserArtifactsAfterDays(Some(9000)).payload(), json!({ "patch": { "storageCleanup": { "browserArtifactsAfterDays": 3650 } } }));
    assert_eq!(MaintenanceChange::LogsAfterDays(None).payload(), json!({ "patch": { "storageCleanup": { "logsAfterDays": null } } }));
    assert_eq!(
        MaintenanceChange::UsageHistoryRetentionDays(UsageHistoryRetentionDays::Days(30)).payload(),
        json!({ "patch": { "usageHistoryRetentionDays": 30 } })
    );
    assert_eq!(
        MaintenanceChange::UsageHistoryRetentionDays(UsageHistoryRetentionDays::Unlimited).payload(),
        json!({ "patch": { "usageHistoryRetentionDays": "unlimited" } })
    );
}

#[test]
fn has_rules_counts_retention_worktree_rules_and_custom_projects() {
    assert!(!has_cleanup_rules(&settings(json!({}))));
    assert!(has_cleanup_rules(&settings(json!({ "storageCleanup": { "logsAfterDays": 3 } }))));
    assert!(has_cleanup_rules(&settings(json!({ "storageCleanup": { "worktreeUnchanged": true } }))));
    // The environment's policy turns its own rules off, but a custom project still counts.
    assert!(!has_cleanup_rules(&settings(json!({ "worktreeCleanup": { "mode": "off" }, "storageCleanup": { "worktreeOnMerge": true } }))));
    let project = json!({ "projectSettingsOverrides": { "p1": { "worktreeCleanup": { "mode": "custom", "rules": { "worktreeAfterDays": 5 } } } } });
    assert!(has_cleanup_rules(&settings(project)));
    let off_project = json!({ "projectSettingsOverrides": { "p1": { "worktreeCleanup": { "mode": "custom", "rules": { "worktreeKeepWhen": "any-local-files" } } } } });
    assert!(!has_cleanup_rules(&settings(off_project)));
}

#[test]
fn profile_option_names_presets_and_custom_intervals() {
    assert_eq!(profile_option(&panel(json!({}))), ProfileOption::Profile(Profile::Balanced));
    assert_eq!(profile_option(&panel(json!({ "backgroundActivity": { "profile": "battery-saver" } }))), ProfileOption::Profile(Profile::BatterySaver));
    // Custom values that happen to equal another preset are that preset.
    let performance = json!({ "backgroundActivity": { "profile": "custom", "baseProfile": "balanced", "overrides": {
        "automaticGitFetchInterval": 15000, "providerHealthRefreshInterval": 60000, "hostPowerMonitorIdleInterval": 120000,
        "pauseWhenHostLowPower": false, "pauseWhenClientLowPower": false } } });
    assert_eq!(profile_option(&panel(performance)), ProfileOption::Profile(Profile::Performance));
    let custom = panel(json!({ "backgroundActivity": { "profile": "custom", "baseProfile": "performance", "overrides": { "automaticGitFetchInterval": 45000 } } }));
    assert_eq!(profile_option(&custom), ProfileOption::Advanced);
    assert_eq!(background_description(&custom), "Uses custom intervals. Shared policy: Performance.");
    assert_eq!(background_description(&panel(json!({}))), "Pauses probes for idle clients, locked hosts, or low power mode.");
}

#[test]
fn reset_shows_while_background_activity_is_not_the_default() {
    assert!(!background_customized(&panel(json!({}))));
    assert!(!background_customized(&panel(json!({ "backgroundActivity": { "schemaVersion": 1, "profile": "balanced", "overrides": {} } }))));
    assert!(background_customized(&panel(json!({ "backgroundActivity": { "profile": "performance" } }))));
    assert!(background_customized(&panel(json!({ "backgroundActivity": { "profile": "balanced", "baseProfile": "balanced" } }))));
    assert_eq!(background_reset_payload(), json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "balanced", "overrides": {} } } }));
    assert_eq!(
        background_profile_payload(Profile::BatterySaver),
        json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "battery-saver", "overrides": {} } } })
    );
}

#[test]
fn shared_policy_keeps_custom_intervals_and_drops_preset_ones() {
    assert_eq!(
        shared_policy_payload(&panel(json!({})), Profile::BatterySaver),
        json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "custom", "baseProfile": "battery-saver", "overrides": {} } } })
    );
    let custom = panel(
        json!({ "backgroundActivity": { "profile": "custom", "baseProfile": "performance", "overrides": { "automaticGitFetchInterval": 45000, "pauseWhenHostLocked": true } } }),
    );
    assert_eq!(
        shared_policy_payload(&custom, Profile::Balanced),
        json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "custom", "baseProfile": "balanced", "overrides": { "automaticGitFetchInterval": 45000 } } } })
    );
}

#[test]
fn an_override_pins_every_resolved_value() {
    let payload = override_payload(&panel(json!({ "backgroundActivity": { "profile": "performance" } })), Override::Pause(PauseKey::OnBattery, true));
    assert_eq!(
        payload,
        json!({ "patch": { "backgroundActivity": { "schemaVersion": 1, "profile": "custom", "baseProfile": "performance", "overrides": {
            "automaticGitFetchInterval": 15000, "providerHealthRefreshInterval": 60000,
            "hostPowerMonitorActiveInterval": 30000, "hostPowerMonitorIdleInterval": 120000,
            "pauseWhenHostLocked": true, "pauseWhenHostLowPower": false, "pauseWhenClientLowPower": false, "pauseWhenOnBattery": true,
        } } } })
    );
    let interval = override_payload(&panel(json!({})), HOST_IDLE.change(90));
    assert_eq!(interval["patch"]["backgroundActivity"]["overrides"]["hostPowerMonitorIdleInterval"], 90_000);
}

#[test]
fn interval_fields_round_and_hold_their_minimum() {
    let resolved = panel(json!({})).resolved_background_activity();
    assert_eq!(INTERVAL_FIELDS.map(|field| field.seconds(&resolved)), [30, 300, 30, 300]);
    assert_eq!(HOST_ACTIVE.normalize("2"), Some(5));
    assert_eq!(GIT_FETCH.normalize(" 12.6 "), Some(13));
    assert_eq!(GIT_FETCH.normalize("-4"), Some(0));
    assert_eq!(GIT_FETCH.normalize("soon"), None);
}

fn report(value: Value) -> CleanupReport {
    decode_report(&value).unwrap()
}

#[test]
fn decodes_a_report_and_describes_the_last_run() {
    let decoded = report(json!({
        "trigger": "manual", "startedAt": "2026-11-02T10:00:00.000Z", "finishedAt": "2026-11-02T10:00:00.000Z",
        "entries": [
            { "kind": "worktree", "outcome": "removed", "reason": "Merged", "path": "/w/repo/feature-x", "threadId": "t1", "threadTitle": "Fix it", "bytes": 2048, "files": null },
            { "kind": "worktree", "outcome": "kept", "reason": "Local changes", "path": "/w/repo/other", "threadId": null, "threadTitle": null, "bytes": null, "files": null },
            { "kind": "logs", "outcome": "removed", "reason": "Old", "path": null, "threadId": null, "threadTitle": null, "bytes": 1536, "files": 1 },
            { "kind": "browser-artifacts", "outcome": "failed", "reason": "EACCES", "path": null, "threadId": null, "threadTitle": null, "bytes": null, "files": 3 },
            { "kind": "future-kind", "outcome": "removed", "reason": "?" },
        ],
        "counts": { "removed": 2, "kept": 1, "failed": 1 }, "omittedCount": 0, "bytesFreed": 3584,
    }));
    assert_eq!(decoded.entries.len(), 4, "unknown kinds are skipped");
    assert_eq!(decoded.counts(), "2 removed, 1 kept, 1 failed");
    let now = parse_ms("2026-11-02T12:30:00.000Z");
    assert_eq!(decoded.description(now), "Last run 2h ago. 3.50 KB recovered. 1 item couldn't be removed.");
    let names: Vec<String> = decoded.entries.iter().map(ReportEntry::name).collect();
    assert_eq!(names, ["feature-x", "/w/repo/other", "Rotated logs", "Browser artifacts"]);
    let sizes: Vec<Option<String>> = decoded.entries.iter().map(ReportEntry::size).collect();
    assert_eq!(sizes, [Some("2.00 KB".into()), None, Some("1 file \u{b7} 1.50 KB".into()), None]);
    assert_eq!(decoded.failures().count(), 1);
    assert_eq!(LastRun::None.description(now), "Cleanup hasn't run yet.");
    assert!(decode_report(&Value::Null).is_none(), "null means cleanup never ran");
}

fn parse_ms(iso: &str) -> i64 {
    crate::sidebar::time::parse_iso(iso).unwrap()
}

#[test]
fn format_bytes_matches_the_web() {
    for (bytes, label) in [
        (0., "0 B"),
        (1023.4, "1023 B"),
        (1024., "1.00 KB"),
        (1_048_000., "1023 KB"),
        (1_048_575., "1.00 MB"),
        (15. * 1024. * 1024., "15.0 MB"),
        (3. * 1024f64.powi(4) * 2048., "6144 TB"),
    ] {
        assert_eq!(format_bytes(bytes), label, "{bytes}");
    }
    assert_eq!(format_count(1_234_567.), "1,234,567");
    assert_eq!(format_count(999.), "999");
}

#[test]
fn compaction_is_suggested_only_for_large_unvacuumed_databases() {
    let big = json!({
        "databaseBytes": 5_368_709_120u64,
        "reclaimableBytes": 2_147_483_648u64,
        "incrementalVacuumEnabled": false,
        "compactCommand": "t3 doctor",
        "canReclaim": true,
        "reclaimFailure": "Disk full"
    });
    let recommendation = compaction_recommendation(&big).unwrap();
    assert_eq!(recommendation.title("Studio"), "2.0 GiB on Studio");
    assert!(recommendation.can_reclaim);
    assert_eq!(recommendation.reclaim_minutes(), 2);
    assert_eq!(recommendation.reclaim_description(), "The last attempt failed: Disk full");
    assert_eq!(
        recommendation.reclaim_confirmation("Studio"),
        (
            "Reclaim 2.0 GiB on Studio?".into(),
            "T3 Code on Studio stops, rebuilds its database, and starts again. It will be offline for about 2 min. Running agents stop when it restarts.".into(),
        )
    );
    assert!(compaction_recommendation(&json!({ "reclaimableBytes": 2e9, "incrementalVacuumEnabled": true, "compactCommand": "x" })).is_none());
    assert!(compaction_recommendation(&json!({ "reclaimableBytes": 1e6, "incrementalVacuumEnabled": false, "compactCommand": "x" })).is_none());
}

#[test]
fn retention_draft_defers_save_until_enter_or_edited_blur() {
    let mut draft = RetentionDraft::default();
    assert!(!draft.is_on(None));
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    assert!(draft.is_on(None));
    // Blur without edits or Enter discards without saving.
    assert_eq!(draft.commit(None, "8", 1, false, 1), RetentionCommit::Discarded);
    assert!(!draft.is_on(None));

    // Re-open draft and press Enter explicitly -> commits default 8.
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    assert_eq!(draft.commit(None, "8", 1, true, 2), RetentionCommit::Save(8));
    assert!(draft.is_on(None));
    // Turning off while save is in flight dispatches SaveOff.
    assert_eq!(draft.toggle(None, false), RetentionToggle::SaveOff);
    assert!(!draft.is_on(None));

    // Re-open, edit, and blur -> saves clamped value; Escape before commit discards.
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    draft.mark_edited();
    assert!(draft.cancel());
    assert!(!draft.is_on(None));

    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    draft.mark_edited();
    assert_eq!(draft.commit(None, "9000", 1, false, 3), RetentionCommit::Save(3650));
    // Re-committing while the initial save is still in flight does not double-save,
    // while failing the active save reverts to Closed so the switch turns Off and retries cleanly.
    assert_eq!(draft.commit(None, "9000", 1, true, 4), RetentionCommit::Restore(3650));
    assert!(draft.fail_submit(3));
    assert!(!draft.is_on(None));
    assert!(!draft.fail_submit(3));

    // Committing a newer age before an older save settles keeps the newer pending submission on old failure.
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    draft.mark_edited();
    assert_eq!(draft.commit(None, "30", 1, true, 5), RetentionCommit::Save(30));
    assert_eq!(draft.commit(None, "60", 1, true, 6), RetentionCommit::Save(60));
    assert!(!draft.fail_submit(5), "stale older failure must not drop newer pending 60-day save");
    assert_eq!(draft, RetentionDraft::Submitting { days: 60, seq: 6 });
    assert!(draft.is_on(None));
    assert!(draft.fail_submit(6), "active 60-day failure reverts switch Off");
    assert!(!draft.is_on(None));

    // Toggling Off then re-committing the same day count gets a fresh seq so the stale save cannot revert it.
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    assert_eq!(draft.commit(None, "30", 1, true, 7), RetentionCommit::Save(30));
    assert_eq!(draft.toggle(None, false), RetentionToggle::SaveOff);
    assert_eq!(draft.toggle(None, true), RetentionToggle::OpenDraft(8));
    assert_eq!(draft.commit(None, "30", 1, true, 8), RetentionCommit::Save(30));
    assert!(!draft.fail_submit(7));
    assert!(draft.is_on(None));
    assert!(draft.fail_submit(8));
}

/// Two switches flipped before the first save answers: the second is built from the settings the
/// first left, so it keeps the first's value instead of pinning the old one.
#[test]
fn queued_background_edits_build_on_the_previous_save() {
    use crate::settings::ServerSettingChange;
    let background = |payload: &Value| settings(json!({ "backgroundActivity": payload["patch"]["backgroundActivity"] }));
    let battery = ServerSettingChange::BackgroundActivity(BackgroundEdit::Override(Override::Pause(PauseKey::OnBattery, true)));
    let locked = ServerSettingChange::BackgroundActivity(BackgroundEdit::Override(Override::Pause(PauseKey::HostLocked, false)));
    let first = battery.payload_for(Some(&settings(json!({})))).unwrap();
    let after_first = background(&first);
    let second = locked.payload_for(Some(&after_first)).unwrap();
    let overrides = &second["patch"]["backgroundActivity"]["overrides"];
    assert_eq!(overrides["pauseWhenOnBattery"], true, "the first edit survives the second");
    assert_eq!(overrides["pauseWhenHostLocked"], false);
    let policy = ServerSettingChange::BackgroundActivity(BackgroundEdit::SharedPolicy(Profile::Performance));
    let third = policy.payload_for(Some(&background(&second))).unwrap();
    assert_eq!(third["patch"]["backgroundActivity"]["baseProfile"], "performance");
    assert_eq!(third["patch"]["backgroundActivity"]["overrides"]["pauseWhenOnBattery"], true);
    assert!(policy.payload_for(None).is_none(), "nothing to build from without settings");
}
