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
