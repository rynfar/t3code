use super::*;
use serde_json::json;
fn config(value: Value) -> ServerConfig {
    serde_json::from_value(value).unwrap()
}
fn provider(instance: &str, status: &str, finished: &str) -> ServerProvider {
    serde_json::from_value(json!({"instanceId":instance,"driver":"codex","enabled":true,"installed":true,"status":"ready", "version":"1.0", "checkedAt":"2026-01-01", "versionAdvisory":{"status":"behind_latest","latestVersion":"2.0","canUpdate":true,"updateCommand":"npm update codex"},"updateState":{"status":status,"finishedAt":finished}})).unwrap()
}
#[test]
fn provider_notices_follow_instances_recency_dismissal_and_clear() {
    let default = provider("codex", "failed", "2026-01-01");
    let newer = provider("work", "succeeded", "2026-01-03");
    let active = provider("other", "running", "2026-01-02");
    let empty = HashSet::new();
    assert_eq!(
        provider_pill(&[default.clone(), active], Some("2026-01-02"), &empty)
            .unwrap()
            .title,
        "Updating Codex"
    );
    let success = provider_pill(&[default.clone(), newer], None, &empty).unwrap();
    assert_eq!(success.title, "Codex updated: v1.0");
    assert_eq!(success.dismiss_after_ms, Some(3000));
    assert_eq!(
        provider_pill(std::slice::from_ref(&default), Some("2026-01-02"), &empty)
            .unwrap()
            .tone,
        Tone::Warning
    );
    let failure = provider_pill(std::slice::from_ref(&default), None, &empty).unwrap();
    assert_eq!(failure.title, "Codex v2.0 update failed");
    assert!(provider_pill(&[default], None, &HashSet::from([failure.key])).is_none());
    assert!(provider_pill(&[], None, &empty).is_none());
    let available = provider("codex", "idle", "");
    let pill = provider_pill(std::slice::from_ref(&available), None, &empty).unwrap();
    assert_eq!(pill.title, "Codex: Update available");
    assert!(provider_pill(&[available], None, &HashSet::from([pill.key])).is_none());
    let original = provider("codex", "failed", "2026-01-03");
    let mut state = crate::server_config::ServerConfigState::default();
    state.config = Some(ServerConfig {
        providers: vec![original.clone()],
        ..Default::default()
    });
    let mut refreshed = original;
    refreshed.checked_at.0 = Some("2026-01-04".into());
    assert!(
        !state.apply_providers(vec![refreshed.clone()]),
        "health refreshes leave the sidebar quiet"
    );
    refreshed.update_state.as_mut().unwrap().finished_at = Some("2026-01-05".into());
    assert!(
        state.apply_providers(vec![refreshed]),
        "a new completed attempt changes the notice key"
    );
}
#[test]
fn one_click_requires_compatible_consistent_commands_and_deduplicates() {
    let a = provider("work", "idle", "");
    let b = provider("codex", "idle", "");
    assert_eq!(
        one_click_candidates(&[a.clone(), b.clone()])[0].instance_id,
        "codex"
    );
    let mut conflict = a.clone();
    conflict.version_advisory.as_mut().unwrap().update_command = Some("other".into());
    assert!(one_click_candidates(&[conflict, b.clone()]).is_empty());
    let mut broken = b;
    broken.compatibility_advisory =
        Some(serde_json::from_value(json!({"latestVersionStatus":"broken"})).unwrap());
    assert!(!candidate(&broken));

    let mut work = provider("work", "idle", "");
    work.checked_at.0 = Some("2026-01-02".into());
    let office = provider("office", "idle", "");
    let original = vec![work, office];
    assert_eq!(one_click_candidates(&original)[0].instance_id, "work");
    let mut same_winner = original.clone();
    same_winner[0].checked_at.0 = Some("2026-01-04".into());
    same_winner[1].checked_at.0 = Some("2026-01-03".into());
    assert_eq!(one_click_candidates(&same_winner)[0].instance_id, "work");
    let mut flipped = same_winner.clone();
    flipped[0].checked_at.0 = Some("2026-01-05".into());
    flipped[1].checked_at.0 = Some("2026-01-06".into());
    assert_eq!(one_click_candidates(&flipped)[0].instance_id, "office");
    assert_eq!(original, flipped, "ordinary provider equality intentionally ignores checkedAt");
    let mut with_default = flipped.clone();
    with_default.push(provider("codex", "idle", ""));
    assert_eq!(one_click_candidates(&with_default)[0].instance_id, "codex");
    let mut provider_state = crate::server_config::ServerConfigState::default();
    provider_state.config = Some(ServerConfig { providers: original.clone(), ..Default::default() });
    assert!(!provider_state.apply_providers(same_winner.clone()), "same-winner stamp refresh stays quiet");
    assert_eq!(provider_state.config.as_ref().unwrap().providers.iter().map(|p| p.checked_at.0.as_deref()).collect::<Vec<_>>(), [Some("2026-01-04"), Some("2026-01-03")]);
    let mut snapshot_state = crate::server_config::ServerConfigState::default();
    let original_config = ServerConfig { providers: original, ..Default::default() };
    assert!(snapshot_state.apply(crate::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(original_config.clone()) }));
    let mut same_config = original_config.clone();
    same_config.providers = same_winner;
    assert!(!snapshot_state.apply(crate::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(same_config) }), "same-winner Snapshot keeps its existing unchanged outcome");
    assert_eq!(snapshot_state.settings_revision(), 2);
    assert_eq!(snapshot_state.keybindings_revision(), 2);
    let provider_changed = provider_state.apply_providers(flipped.clone());
    let mut flipped_config = original_config;
    flipped_config.providers = flipped;
    let snapshot_changed = snapshot_state.apply(crate::server_config::ServerConfigStreamEvent::Snapshot { config: Box::new(flipped_config) });
    assert_eq!((provider_changed, snapshot_changed), (true, true), "a relative representative flip must be material for providerStatuses and Snapshot");
    assert_eq!(one_click_candidates(&provider_state.config.as_ref().unwrap().providers)[0].instance_id, "office");
    assert_eq!(one_click_candidates(&snapshot_state.config.as_ref().unwrap().providers)[0].instance_id, "office");
    assert_eq!(snapshot_state.settings_revision(), 3);
    assert_eq!(snapshot_state.keybindings_revision(), 3);
}
#[test]
fn planner_orders_merges_and_skips_web_restrictions() {
    let a = config(
        json!({"environment":{"serverVersion":"0.1.2","capabilities":{"serverSelfUpdate":"boot-service"}},"providers":[serde_json::json!({"instanceId":"codex","driver":"codex","enabled":true,"installed":true,"status":"ready","versionAdvisory":{"status":"behind_latest","latestVersion":"2","canUpdate":true,"updateCommand":"update"}})]}),
    );
    let manual = config(json!({"environment":{"serverVersion":"0.1.2"}}));
    let nightly = config(
        json!({"environment":{"serverVersion":"0.1.2-nightly.20261001.1","capabilities":{"serverSelfUpdate":"boot-service"}}}),
    );
    let machine = |id, config| Machine {
        id,
        label: id,
        config: Some(config),
        connected: true,
        hosted_by_this_app: false,
        can_maintain: true,
        can_manage_providers: true,
        updating: false,
    };
    let plan = collect(
        &[
            machine("main", &a),
            machine("manual", &manual),
            machine("nightly", &nightly),
        ],
        "0.1.6",
    );
    assert_eq!(plan.servers.len(), 1);
    assert_eq!(plan.providers.len(), 1);
    assert_eq!(plan.summary(), "Update 1 server and 1 provider");
    assert_eq!(plan.title(), "Update 1 machine?");
    assert_eq!(plan.rows[0].providers, ["Codex"]);
    assert_eq!(
        plan.rows[1].skipped.as_deref(),
        Some("needs a manual update")
    );
    assert_eq!(
        plan.rows[2].skipped.as_deref(),
        Some("on the nightly channel")
    );
    assert_eq!(plan.restarts.len(), 1);
    assert!(
        !plan.restarts[0].continues_threads,
        "old servers do not advertise continuation"
    );
    assert_eq!(channel("0.1.6-beta.1"), "stable");
    assert_eq!(channel("0.1.6-nightly.1"), "stable");
    assert_eq!(channel("0.1.6-preview.20261001.12"), "preview");
    assert!(!behind("0.1.7", "0.1.6"));
    assert!(!behind("0.1.6-preview.1", "0.1.6"));
    assert!(behind("0.1.6-nightly.1", "0.1.6-nightly.2"));
    assert!(behind("0.1.6-nightly.9", "0.1.6-nightly.10"));
}
#[test]
fn policies_restarts_and_running_threads_follow_the_web() {
    assert_eq!(
        managed_denial(
            Some(&json!({"organization":"Corp","limits":{"updates":{"mode":"off"}}})),
            None
        )
        .as_deref(),
        Some("Corp manages T3 Code updates.")
    );
    assert!(
        managed_denial(
            Some(&json!({"organization":"Corp","limits":{"updates":{"allowNightly":false}}})),
            Some("0.1.6-nightly.20261001.1")
        )
        .unwrap()
        .contains("Only stable")
    );
    let c = config(json!({"settings":{"continueThreadsAfterServerUpdate":true}}));
    let machine = Machine {
        id: "local",
        label: "Local",
        config: Some(&c),
        connected: true,
        hosted_by_this_app: true,
        can_maintain: true,
        can_manage_providers: true,
        updating: false,
    };
    assert!(collect_restarting(&[machine], &[], false).is_empty());
    let machine = Machine {
        id: "local",
        label: "Local",
        config: Some(&c),
        connected: true,
        hosted_by_this_app: true,
        can_maintain: true,
        can_manage_providers: true,
        updating: false,
    };
    assert!(collect_restarting(&[machine], &[], true)[0].continues_threads);
    assert_eq!(
        [
            "preparing",
            "starting",
            "running",
            "stopping",
            "idle",
            "error"
        ]
        .into_iter()
        .filter(|s| running(s))
        .count(),
        3
    );
    assert_eq!(
        thread_warning(1, 2).unwrap(),
        "3 running threads will be interrupted. 2 can resume after the restart."
    );
    assert!(thread_warning(0, 1).unwrap().contains("can resume"));
    assert!(thread_warning(0, 0).is_none());
    let restarts = [
        RestartingMachine {
            environment_id: "a".into(),
            continues_threads: false,
        },
        RestartingMachine {
            environment_id: "b".into(),
            continues_threads: true,
        },
    ];
    assert_eq!(
        running_counts(
            &restarts,
            [
                ("a", "preparing"),
                ("a", "starting"),
                ("b", "running"),
                ("b", "idle"),
                ("other", "running")
            ]
        ),
        [("a".into(), 2), ("b".into(), 1)]
    );
}
#[test]
fn a_handoff_requires_matching_committed_lifecycle_outcome() {
    assert!(ready_outcome(&json!({}), &json!({"type":"ready"})).is_none());
    let result = json!({"updateId":"u","targetVersion":"0.1.6"});
    assert!(
        ready_outcome(
            &result,
            &json!({"type":"ready","payload":{"environment":{"serverVersion":"0.1.6"}}})
        )
        .is_none()
    );
    for (status, version, ok) in [
        ("committed", "0.1.6", true),
        ("rolled-back", "0.1.2", false),
        ("committed", "0.1.2", false),
    ] {
        let event = json!({"type":"ready","payload":{"environment":{"serverVersion":version},"updateOutcome":{"id":"u","targetVersion":"0.1.6","status":status}}});
        assert_eq!(ready_outcome(&result, &event).unwrap().is_ok(), ok);
    }
}

#[test]
fn channel_switch_plans_track_only_checks_migrations_and_enforces_host_rules() {
    let nightly = "0.1.4-nightly.20261027.3796";
    let old_nightly = "0.1.3-nightly.20261001.3500";
    let newer_nightly = "0.1.4-nightly.20261020.3700";
    let host = |version: &str, self_update: Option<&str>, os: &str, mig: Option<u64>, ch_switch: Option<bool>, policy: Option<Value>| {
        config(json!({
            "environment": {
                "serverVersion": version,
                "platform": { "os": os, "machine": "x86_64" },
                "databaseMigration": mig,
                "capabilities": {
                    "serverSelfUpdate": self_update,
                    "desktopAppUpdate": ch_switch.unwrap_or(false),
                    "desktopAppChannelSwitch": ch_switch
                }
            },
            "managedPolicy": policy
        }))
    };
    let m = |id, cfg| Machine {
        id,
        label: id,
        config: Some(cfg),
        connected: true,
        hosted_by_this_app: false,
        can_maintain: true,
        can_manage_providers: false,
        updating: false,
    };

    let this_app = host(nightly, Some("desktop-managed"), "linux", Some(72), Some(true), None);
    let old_service = host(old_nightly, Some("boot-service"), "linux", Some(69), None, None);
    let newer_service = host(newer_nightly, Some("boot-service"), "linux", Some(69), None, None);
    let linux_desktop = host(old_nightly, Some("desktop-managed"), "linux", Some(69), Some(true), None);
    let new_service = host(nightly, Some("boot-service"), "linux", Some(72), None, None);
    let old_server = host("0.1.2-nightly.20260901.3000", Some("boot-service"), "linux", None, None, None);
    let mac_desktop = host(old_nightly, Some("desktop-managed"), "darwin", Some(69), Some(true), None);
    let preview = host("0.1.3-preview.20261026.3702", Some("boot-service"), "linux", Some(69), None, None);
    let stable_service = host("0.1.3", Some("boot-service"), "linux", Some(69), None, None);
    let stable_desktop = host("0.1.2", Some("desktop-managed"), "linux", Some(68), Some(true), None);
    let current_desktop = host("0.1.3", Some("desktop-managed"), "darwin", Some(69), Some(true), None);

    let stable = ChannelSwitchRelease {
        channel: UpdateTrack::Latest,
        version: "0.1.3".into(),
        database_migration: Some(69),
    };
    let mut hosted = m("this-app", &this_app);
    hosted.hosted_by_this_app = true;
    let machines = [
        hosted,
        m("old-service", &old_service),
        m("newer-service", &newer_service),
        m("linux-desktop", &linux_desktop),
        m("new-service", &new_service),
        m("old-server", &old_server),
        m("mac-desktop", &mac_desktop),
        m("preview", &preview),
        m("stable", &stable_service),
        m("stable-desktop", &stable_desktop),
        m("current-desktop", &current_desktop),
    ];
    let switched = collect_channel_switch(&machines, &stable);
    let summary: Vec<_> = switched
        .targets
        .iter()
        .map(|t| (t.server.label.as_str(), t.server.target_version.as_str(), t.channel, t.track_only))
        .collect();
    assert_eq!(
        summary,
        [
            ("old-service", "0.1.3", None, false),
            ("linux-desktop", "0.1.3", Some(UpdateTrack::Latest), false),
            ("stable-desktop", "0.1.3", Some(UpdateTrack::Latest), false),
            ("current-desktop", "0.1.3", Some(UpdateTrack::Latest), true),
        ]
    );
    assert!(switched.targets.iter().all(|t| t.target_database_migration == Some(69)));
    assert_eq!(
        switched.plan.rows.iter().filter(|r| r.app.is_some()).map(|r| r.label.as_str()).collect::<Vec<_>>(),
        ["old-service", "linux-desktop", "stable-desktop"],
        "track-only desktop hosts do not show up as moving rows or restarts"
    );
    assert_eq!(
        switched.plan.servers.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
        ["old-service", "linux-desktop", "stable-desktop"],
        "track-only desktop hosts stay out of plan.servers so capsule machines match moving rows"
    );
    let skipped: Vec<_> = switched
        .plan
        .rows
        .iter()
        .filter_map(|r| r.skipped.as_deref().map(|reason| (r.label.as_str(), reason)))
        .collect();
    assert_eq!(
        skipped,
        [
            ("newer-service", "run `t3 update --channel stable --allow-downgrade` there"),
            ("new-service", "its database has changes 0.1.3 does not support yet; stay on Nightly until Stable has them"),
            ("old-server", "its server predates database checks; update it on Nightly first"),
            ("mac-desktop", "switch the Mac app on that Mac"),
            ("preview", "runs a preview build"),
        ]
    );

    let nightly_release = ChannelSwitchRelease {
        channel: UpdateTrack::Nightly,
        version: nightly.into(),
        database_migration: Some(72),
    };
    let old_desktop = host("0.1.2", Some("desktop-managed"), "linux", Some(68), None, None);
    let no_nightly = host(
        "0.1.2",
        Some("boot-service"),
        "linux",
        Some(70),
        None,
        Some(json!({"organization":"Corp","limits":{"updates":{"mode":"auto","allowNightly":false}}})),
    );
    let ahead = host("0.1.3", Some("boot-service"), "linux", Some(73), None, None);
    let to_nightly = collect_channel_switch(
        &[
            m("service", &stable_desktop),
            m("ahead", &ahead),
            m("old-desktop", &old_desktop),
            m("no-nightly", &no_nightly),
        ],
        &nightly_release,
    );
    assert_eq!(to_nightly.targets.len(), 1);
    assert_eq!(to_nightly.targets[0].server.label, "service");
    let nightly_skipped: Vec<_> = to_nightly
        .plan
        .rows
        .iter()
        .filter_map(|r| r.skipped.as_deref().map(|reason| (r.label.as_str(), reason)))
        .collect();
    assert_eq!(
        nightly_skipped,
        [
            ("ahead", "its database has changes 0.1.4-nightly.20261027.3796 does not support yet; stay on Stable until Nightly has them"),
            ("old-desktop", "needs a newer desktop app"),
            ("no-nightly", "Corp manages T3 Code updates. Only stable releases can be installed."),
        ]
    );
    assert!(awaits_track_switch(&m("old-service", &old_service), UpdateTrack::Latest));
    assert!(!awaits_track_switch(&m("preview", &preview), UpdateTrack::Latest));
}

#[test]
fn capsule_progress_slow_retry_dismiss_and_saved_results_match_web() {
    assert_eq!(format_update_elapsed(-5), "<1m");
    assert_eq!(format_update_elapsed(59_999), "<1m");
    assert_eq!(format_update_elapsed(180_000), "3m");
    assert_eq!(format_update_elapsed(65 * 60_000), "1h 5m");

    let plan = Plan {
        servers: vec![
            ServerTarget {
                environment_id: "a".into(),
                label: "Alpha".into(),
                target_version: "1.0.0".into(),
                self_update: "boot-service".into(),
            },
            ServerTarget {
                environment_id: "b".into(),
                label: "Beta".into(),
                target_version: "1.0.0".into(),
                self_update: "respawn".into(),
            },
        ],
        providers: vec![ProviderTarget {
            environment_id: "a".into(),
            driver: "codex".into(),
            instance_id: "codex".into(),
        }],
        rows: vec![
            Row {
                environment_id: "a".into(),
                label: "Alpha".into(),
                app: Some("Server".into()),
                providers: vec!["Codex".into()],
                skipped: None,
            },
            Row {
                environment_id: "b".into(),
                label: "Beta".into(),
                app: Some("Server".into()),
                providers: vec![],
                skipped: None,
            },
        ],
        restarts: vec![],
    };
    let mut run = Some(UpdateEverythingRun::from_plan(&plan, false, 1_000));
    let stages = vec![("b".into(), Some(ServerUpdateStage::Downloading))];
    let cap = describe_update_capsule(run.as_ref().unwrap(), &stages, 1_000);
    assert_eq!(cap.tone, CapsuleTone::Default);
    assert_eq!(cap.label, "0 of 2 updated");
    assert_eq!(cap.action, None);

    // Slow >3m warns on the slowest active machine.
    let slow = describe_update_capsule(run.as_ref().unwrap(), &stages, 1_000 + UPDATE_SLOW_AFTER_MS);
    assert_eq!(slow.tone, CapsuleTone::Slow);
    assert_eq!(slow.label, "Alpha");
    assert_eq!(slow.slow, Some(("updating providers".into(), "3m".into())));

    // Settling server a as done and b as failed enables Retry.
    begin_servers_phase(&mut run, 5_000);
    settle_machine(&mut run, "a", None, true, Some(None));
    settle_machine(&mut run, "b", Some("offline".into()), false, Some(None));
    let failed = describe_update_capsule(run.as_ref().unwrap(), &[], 6_000);
    assert_eq!(failed.tone, CapsuleTone::Failed);
    assert_eq!(failed.label, "Beta failed");
    assert_eq!(failed.action, Some(CapsuleAction::Retry));

    // Track switch failures dismiss instead of retrying directly.
    let mut track_run = run.clone();
    track_run.as_mut().unwrap().track_switch = true;
    assert_eq!(
        describe_update_capsule(track_run.as_ref().unwrap(), &[], 6_000).action,
        Some(CapsuleAction::Dismiss)
    );

    // Retry resets only failed server machines.
    let retried = retry_failed_servers(&mut run, 10_000);
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].0.environment_id, "b");
    settle_machine(&mut run, "b", None, true, None);
    assert!(run.is_none(), "all-done Servers phase auto-closes run");

    // Retry skips failed servers already reconnected on the target version.
    let mut already_updated_run = Some(UpdateEverythingRun::from_plan(&plan, false, 1_000));
    begin_servers_phase(&mut already_updated_run, 2_000);
    settle_machine(&mut already_updated_run, "a", Some("timeout".into()), false, None);
    settle_machine(&mut already_updated_run, "b", Some("offline".into()), false, None);
    let retried = retry_failed_servers_with_versions(&mut already_updated_run, 10_000, |id| {
        match id {
            "a" => Some("1.0.0"),
            "b" => Some("0.9.0"),
            _ => None,
        }
    });
    assert_eq!(
        retried
            .iter()
            .map(|(server, _)| server.environment_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
    let current = already_updated_run.as_ref().unwrap();
    assert_eq!(current.machines[0].result, Some(MachineResult::Done));
    assert!(current.machines[0].server.is_none());
    assert_eq!(current.machines[1].result, None);
    assert!(current.machines[1].server.is_some());

    // Dismissing settled machines clears the run when none remain.
    let mut dismiss_run = Some(UpdateEverythingRun::from_plan(&plan, false, 1_000));
    begin_servers_phase(&mut dismiss_run, 2_000);
    settle_machine(&mut dismiss_run, "a", Some("denied".into()), false, None);
    settle_machine(&mut dismiss_run, "b", None, true, None);
    dismiss_settled_machines(&mut dismiss_run);
    assert!(dismiss_run.is_none());

    // Toast formatting + saved result freshness.
    let result = collect_update_everything_result(
        &[
            ("Alpha", &ServerOutcome::Updated),
            ("Beta", &ServerOutcome::Failed("timed out".into())),
        ],
        &[ProviderOutcome::Updated],
    );
    let toast = update_everything_toast_view(&result).unwrap();
    assert!(toast.is_error);
    assert_eq!(toast.title, "Updated 1 server and 1 provider");
    assert_eq!(toast.description.as_deref(), Some("Beta: timed out"));

    let mut handing_off = Some(UpdateEverythingRun::from_plan(&plan, false, 1_000));
    handing_off.as_mut().unwrap().machines[0].handed_off_version = Some("1.0.0".into());
    let saved = SavedUpdateEverythingResult::from_run(handing_off.as_ref(), &[], &result, 5_000);
    assert_eq!(saved.handed_off.len(), 1);
    let raw = serde_json::to_value(&saved).unwrap();
    assert!(decode_fresh_saved_result(raw.clone(), 5_000 + SAVED_RESULT_MAX_AGE_MS - 1).is_some());
    assert!(decode_fresh_saved_result(raw, 5_000 + SAVED_RESULT_MAX_AGE_MS).is_none());
    let resumed = saved.into_resuming_run().unwrap();
    assert_eq!(
        resolve_machine_status(&resumed.machines[0], resumed.phase, None),
        UpdateRunMachineStatus::Restarting
    );

    let restarting_only = UpdateEverythingResult {
        restarting: vec!["Alpha".into()],
        ..Default::default()
    };
    let relaunch_toast = update_everything_toast_view(&restarting_only).unwrap();
    assert!(!relaunch_toast.is_error);
    assert_eq!(relaunch_toast.title, "Alpha is still restarting.");
    assert!(relaunch_toast.description.is_none());
    assert!(update_everything_toast_view(&UpdateEverythingResult::default()).is_none());
}

#[test]
fn provider_pill_sorts_by_recency_and_ties_favor_failure_over_warning_and_success() {
    let failed = provider("codex", "failed", "2026-01-02T00:00:00Z");
    let mut unchanged = provider("work", "unchanged", "2026-01-02T00:00:00Z");
    unchanged.driver = "claudeAgent".into();
    let mut newer_success = provider("other", "succeeded", "2026-01-03T00:00:00Z");
    newer_success.driver = "gemini".into();
    let empty = HashSet::new();
    // Tied timestamp: failed beats unchanged.
    assert_eq!(
        provider_pill(&[unchanged.clone(), failed.clone()], None, &empty).unwrap().tone,
        Tone::Error
    );
    // Newer timestamp: succeeded beats older failed until dismissed, then failed remains visible.
    let pill = provider_pill(&[failed.clone(), newer_success.clone()], None, &empty).unwrap();
    assert_eq!(pill.tone, Tone::Success);
    let after_dismiss = provider_pill(&[failed, newer_success], None, &HashSet::from([pill.key])).unwrap();
    assert_eq!(after_dismiss.tone, Tone::Error);
}
