use serde_json::{Value, json};

use super::*;

const FRESH: FirstRunEvidence = FirstRunEvidence {
    completed: false,
    live: true,
    provenance_authoritative: true,
    server_config_available: true,
    workspace_fresh: true,
    project_count: 1,
    thread_count: 1,
};

#[test]
fn opens_the_wizard_for_an_authoritative_fresh_workspace() {
    assert_eq!(resolve_first_run_decision(&FRESH), resolution(FirstRunDecision::Wizard, false));
}

#[test]
fn cached_project_counts_skip_the_wizard_without_completing_onboarding() {
    let cached = FirstRunEvidence { live: false, project_count: 3, workspace_fresh: false, ..FRESH };
    assert_eq!(resolve_first_run_decision(&cached), resolution(FirstRunDecision::App, false));
}

#[test]
fn completion_is_backfilled_once_the_server_confirms_existing_projects() {
    let established = FirstRunEvidence { project_count: 3, workspace_fresh: false, ..FRESH };
    assert_eq!(resolve_first_run_decision(&established), resolution(FirstRunDecision::App, true));
    let used = FirstRunEvidence { workspace_fresh: false, ..FRESH };
    assert_eq!(resolve_first_run_decision(&used), resolution(FirstRunDecision::App, true));
}

#[test]
fn a_pending_welcome_waits_and_a_completed_one_opens_the_wizard() {
    let pending = ServerWelcome { bootstrap_status: Some("pending".into()), ..Default::default() };
    let waiting = FirstRunEvidence { provenance_authoritative: pending.is_authoritative(), ..FRESH };
    assert_eq!(resolve_first_run_decision(&waiting), resolution(FirstRunDecision::Pending, false));
    let complete = ServerWelcome { bootstrap_status: Some("complete".into()), ..Default::default() };
    let ready = FirstRunEvidence { provenance_authoritative: complete.is_authoritative(), ..FRESH };
    assert_eq!(resolve_first_run_decision(&ready), resolution(FirstRunDecision::Wizard, false));
    // A legacy welcome without a bootstrap status is authoritative.
    assert!(ServerWelcome::default().is_authoritative());
}

#[test]
fn completed_onboarding_does_not_wait_for_server_data() {
    let completed = FirstRunEvidence { completed: true, live: false, server_config_available: false, ..FRESH };
    assert_eq!(resolve_first_run_decision(&completed), resolution(FirstRunDecision::App, false));
}

#[test]
fn a_first_launch_with_nothing_saved_opens_the_wizard() {
    assert_eq!(resolve_unpaired(false, 0), FirstRunDecision::Wizard);
    assert_eq!(resolve_unpaired(false, 2), FirstRunDecision::Pending);
    assert_eq!(resolve_unpaired(true, 0), FirstRunDecision::App);
}

#[test]
fn lifecycle_items_yield_only_the_welcome() {
    let welcome = json!({
        "version": 1, "sequence": 1, "type": "welcome",
        "payload": {
            "environment": {"environmentId": "env"}, "cwd": "/projects/current", "projectName": "current",
            "bootstrapProjectId": "p1", "bootstrapThreadId": "t1", "bootstrapProjectCreated": true,
        },
    });
    let decoded = lifecycle_welcome(&welcome).unwrap();
    assert_eq!(decoded.bootstrap_project_id.as_deref(), Some("p1"));
    assert_eq!(decoded.bootstrap_thread_created, None);
    assert!(lifecycle_welcome(&json!({"version": 1, "sequence": 2, "type": "ready", "payload": {}})).is_none());
}

fn project(id: &str, workspace_root: &str) -> ProjectShell {
    serde_json::from_value(json!({
        "id": id, "title": id, "workspaceRoot": workspace_root,
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
    }))
    .unwrap()
}

fn thread(id: &str, project_id: &str, extra: Value) -> ThreadShell {
    let mut value = json!({
        "id": id, "projectId": project_id, "title": id, "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "runtimeMode": "full-access", "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": null, "activeRunId": null, "status": "idle", "latestUserMessageAt": null,
        "hasActionableProposedPlan": false, "pendingRuntimeRequest": null, "latestVisibleMessage": null,
        "itemCount": 0, "visibleItemCount": 0, "settledOverride": null, "settledAt": null, "deletedAt": null,
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z", "archivedAt": null,
    });
    value.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).unwrap()
}

fn bootstrap_welcome() -> ServerWelcome {
    ServerWelcome {
        bootstrap_status: None,
        bootstrap_project_id: Some("bootstrap-project".into()),
        bootstrap_thread_id: Some("bootstrap-thread".into()),
        bootstrap_project_created: Some(true),
        bootstrap_thread_created: Some(true),
    }
}

#[test]
fn only_the_unused_project_and_thread_created_from_the_server_cwd_are_fresh() {
    let projects = [project("bootstrap-project", "/projects/current")];
    let unused = thread("bootstrap-thread", "bootstrap-project", json!({}));
    assert!(is_fresh_first_run_workspace(Some("/projects/current/"), &bootstrap_welcome(), &projects, &[&unused]));
    assert!(is_fresh_first_run_workspace(None, &ServerWelcome::default(), &[], &[]));

    // An existing cwd project and thread that startup reused.
    let reused = ServerWelcome { bootstrap_project_created: Some(false), bootstrap_thread_created: Some(false), ..bootstrap_welcome() };
    assert!(!is_fresh_first_run_workspace(Some("/projects/current"), &reused, &projects, &[&unused]));
    // An older server that omits creation provenance.
    let legacy = ServerWelcome { bootstrap_project_created: None, bootstrap_thread_created: None, ..bootstrap_welcome() };
    assert!(!is_fresh_first_run_workspace(Some("/projects/current"), &legacy, &projects, &[&unused]));
    // A project somewhere other than the server's cwd.
    assert!(!is_fresh_first_run_workspace(Some("/projects/other"), &bootstrap_welcome(), &projects, &[&unused]));
}

#[test]
fn windows_paths_are_normalized_before_comparing_the_bootstrap_workspace() {
    let projects = [project("bootstrap-project", "c:/projects/current")];
    let unused = thread("bootstrap-thread", "bootstrap-project", json!({}));
    assert!(is_fresh_first_run_workspace(Some("C:\\Projects\\Current\\"), &bootstrap_welcome(), &projects, &[&unused]));
}

#[test]
fn a_bootstrap_thread_that_was_used_is_not_fresh() {
    let projects = [project("bootstrap-project", "/projects/current")];
    for used in [json!({"latestUserMessageAt": "2026-08-23T12:00:00.000Z"}), json!({"latestRunId": "run-1"}), json!({"activeProviderThreadId": "provider-thread"})] {
        let thread = thread("bootstrap-thread", "bootstrap-project", used);
        assert!(!is_fresh_first_run_workspace(Some("/projects/current"), &bootstrap_welcome(), &projects, &[&thread]));
    }
}

fn provider(instance_id: &str, driver: &str, extra: Value) -> ServerProvider {
    let mut value = json!({
        "instanceId": instance_id, "driver": driver, "enabled": true, "installed": true, "version": "1.0.0",
        "status": "ready", "auth": {"status": "unknown"}, "checkedAt": "2026-08-23T00:00:00.000Z", "models": [],
    });
    value.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    serde_json::from_value(value).unwrap()
}

#[test]
fn agents_are_ready_only_when_their_provider_is() {
    let state = |extra: Value| agent_state(Some(&provider("codex", "codex", extra)));
    assert_eq!(state(json!({"auth": {"status": "authenticated"}})), AgentState::Ready);
    assert_eq!(state(json!({"auth": {"status": "authenticated"}, "status": "error"})), AgentState::Attention);
    assert_eq!(state(json!({"auth": {"status": "authenticated"}, "status": "warning"})), AgentState::Attention);
    // Sign-in is offered only when the server reports an authentication failure.
    assert_eq!(state(json!({"status": "error", "auth": {"status": "unauthenticated"}})), AgentState::SignIn);
    assert_eq!(state(json!({"status": "error"})), AgentState::Attention);
    // Disabled providers offer neither install nor sign-in.
    assert_eq!(state(json!({"enabled": false, "installed": false})), AgentState::Disabled);
    assert_eq!(state(json!({"status": "disabled"})), AgentState::Disabled);
    assert_eq!(state(json!({"installed": false, "status": "error"})), AgentState::Install);
    assert_eq!(state(json!({"installed": false, "status": "warning"})), AgentState::Checking);
    assert_eq!(agent_state(None), AgentState::Checking);
}

#[test]
fn the_most_usable_instance_stands_for_its_driver() {
    let ready = provider("codex", "codex", json!({}));
    let signed_out = provider("codex_work", "codex", json!({"status": "error", "auth": {"status": "unauthenticated"}}));
    let failed = provider("codex", "codex", json!({"status": "error"}));
    let disabled = provider("codex", "codex", json!({"enabled": false}));
    let missing = provider("codex_work", "codex", json!({"installed": false, "status": "error"}));
    let id = |providers: &[ServerProvider]| best_instance(providers, "codex").map(|provider| provider.instance_id.clone());
    assert_eq!(id(&[signed_out.clone(), ready.clone()]), Some("codex".into()));
    assert_eq!(id(&[failed, signed_out]), Some("codex_work".into()));
    assert_eq!(best_instance(&[disabled, missing, ready.clone()], "codex"), Some(&ready));
    assert_eq!(id(&[]), None);
}

#[test]
fn the_agents_step_lists_every_codex_instance_and_the_best_claude() {
    let providers = [
        provider("codex", "codex", json!({})),
        provider("codex_work", "codex", json!({"displayName": "Work"})),
        provider("claude_old", "claudeAgent", json!({"installed": false, "status": "error"})),
        provider("claudeAgent", "claudeAgent", json!({})),
        provider("cursor", "cursor", json!({})),
    ];
    let listed = |policy: Option<&Value>| {
        onboarding_agents(&providers, policy).iter().map(|agent| (agent.driver, agent.provider.map(|provider| provider.instance_id.as_str()))).collect::<Vec<_>>()
    };
    assert_eq!(listed(None), [("codex", Some("codex")), ("codex", Some("codex_work")), ("claudeAgent", Some("claudeAgent"))]);
    let claude_only = json!({"organization": "Acme", "limits": {"providers": {"allowedDrivers": ["claudeAgent"]}}});
    assert_eq!(listed(Some(&claude_only)), [("claudeAgent", Some("claudeAgent"))]);
    // Before the server reports providers, each driver shows as checking.
    let empty = onboarding_agents(&[], None);
    assert_eq!(empty.iter().map(|agent| (agent.driver, agent.provider.is_none())).collect::<Vec<_>>(), [("codex", true), ("claudeAgent", true)]);
    assert_eq!(agent_display_name("claudeAgent", None), "Claude Code");
    assert_eq!(agent_display_name("codex", None), "Codex");
    assert_eq!(agent_display_name("codex", Some(&providers[1])), "Work");
}

#[test]
fn the_privacy_line_names_an_administrator_that_turned_usage_data_off() {
    assert!(privacy_notice(None).starts_with("T3 Code collects anonymous usage data to help us improve it."));
    assert!(privacy_notice(None).ends_with("and how to opt out, see our"));
    let off = json!({"organization": "Acme", "limits": {"telemetry": false}});
    assert_eq!(privacy_notice(Some(&off)), "Acme turned off anonymous usage data. To read more about how your data is used, see our");
}

fn settings_with_binary(instance_id: &str, driver: &str, binary_path: &str) -> ServerSettings {
    serde_json::from_value(json!({"providerInstances": {instance_id: {"driver": driver, "config": {"binaryPath": binary_path}}}})).unwrap()
}

#[test]
fn login_commands_use_and_quote_the_configured_binary() {
    let work = provider("codex_work", "codex", json!({}));
    assert_eq!(login_command(&work, &settings_with_binary("codex_work", "codex", "/opt/codex-work/bin/codex"), "linux"), "/opt/codex-work/bin/codex login");
    let codex = provider("codex", "codex", json!({}));
    let windows = settings_with_binary("codex", "codex", "C:\\Program Files\\Codex & Tools\\codex.exe");
    assert_eq!(login_command(&codex, &windows, "windows"), "& 'C:\\Program Files\\Codex & Tools\\codex.exe' login");
    let claude = provider("claude", "claudeAgent", json!({}));
    let odd = settings_with_binary("claude", "claudeAgent", "/opt/Claude Tools/$current/claude");
    assert_eq!(login_command(&claude, &odd, "linux"), "'/opt/Claude Tools/$current/claude' auth login");
    assert_eq!(login_command(&claude, &ServerSettings::default(), "darwin"), "claude auth login");
    // An old server that reports an unknown shell gets the default command.
    assert_eq!(login_command(&codex, &settings_with_binary("codex", "codex", "/opt/Codex Tools/codex"), "unknown"), "codex login");
}

#[test]
fn the_home_prefix_stays_expandable_while_the_rest_is_quoted() {
    let codex = provider("codex", "codex", json!({}));
    for (binary_path, expected) in [
        ("~/my tools/codex", "~/'my tools/codex' login"),
        ("~\\my tools/codex", "~/'my tools/codex' login"),
        ("~/tools/codex's build", r#"~/'tools/codex'"'"'s build' login"#),
        ("~\\tools\\codex's build", r#"~/'tools\codex'"'"'s build' login"#),
        ("~/tools/codex; echo unsafe", "~/'tools/codex; echo unsafe' login"),
    ] {
        assert_eq!(login_command(&codex, &settings_with_binary("codex", "codex", binary_path), "linux"), expected, "{binary_path}");
    }
}

#[test]
fn install_commands_follow_the_server_platform() {
    assert_eq!(install_command("claudeAgent", "darwin"), Some("curl -fsSL https://claude.ai/install.sh | bash"));
    assert_eq!(install_command("codex", "windows"), Some("irm https://chatgpt.com/codex/install.ps1 | iex"));
    assert_eq!(install_command("cursor", "linux"), None);
}
