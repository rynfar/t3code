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
    assert_eq!(
        privacy_notice(None),
        "T3 Code collects anonymous usage data to improve the app. To opt out, see the"
    );
    let off = json!({"organization": "Acme", "limits": {"telemetry": false}});
    assert_eq!(
        privacy_notice(Some(&off)),
        "Acme turned off anonymous usage data. See the"
    );
    let unnamed = json!({"limits": {"telemetry": false}});
    assert_eq!(
        privacy_notice(Some(&unnamed)),
        "Your organization turned off anonymous usage data. See the"
    );
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

#[test]
fn welcome_page_groups_agents_by_readiness_and_detects_cli_path_status() {
    let claude = provider("claude", "claudeAgent", json!({ "status": "ready", "installed": true, "auth": { "status": "authenticated" } }));
    let codex = provider("codex", "codex", json!({ "status": "warning", "installed": true, "auth": { "status": "unauthenticated" } }));
    let cursor = provider("cursor", "cursor", json!({ "status": "warning", "installed": false, "auth": { "status": "unknown" } }));
    let broken = provider("codex-old", "codex", json!({
        "displayName": "Old Codex",
        "status": "ready",
        "installed": true,
        "auth": { "status": "authenticated" },
        "compatibilityAdvisory": { "status": "broken" }
    }));
    let list = [cursor, codex, broken, claude];
    let grouped = group_welcome_agents(Some(&list), None, None);
    assert_eq!(grouped.ready_count, 1);
    assert!(grouped.checking);
    let found_ids: Vec<&str> = grouped.found.iter().map(|p| p.instance_id.as_str()).collect();
    assert_eq!(found_ids, vec!["claude", "codex", "codex-old"]);
    let avail_ids: Vec<&str> = grouped.available.iter().map(|p| p.instance_id.as_str()).collect();
    assert_eq!(avail_ids, vec!["cursor"]);

    let status = detect_cli_command_status(
        std::path::Path::new("/Users/me/.t3/bin"),
        true,
        &[("/usr/local/bin", false), ("/usr/bin", false)],
        &ShellEnv { platform: "macos", home: Some("/Users/me"), shell: Some("/bin/zsh"), ..Default::default() },
    );
    assert!(!status.on_path);
    assert_eq!(status.profile_file.as_deref(), Some("~/.zshrc"));
    assert_eq!(status.path_line.as_deref(), Some("export PATH=\"$PATH:$HOME/.t3/bin\""));
    assert!(status.description().contains("not on your PATH yet"));

    let fish = detect_cli_command_status(
        std::path::Path::new("/Users/me/.t3/bin"),
        true,
        &[("/usr/bin", false)],
        &ShellEnv { platform: "macos", home: Some("/Users/me"), shell: Some("/opt/homebrew/bin/fish"), ..Default::default() },
    );
    assert_eq!(fish.profile_file.as_deref(), Some("~/.config/fish/config.fish"));
    assert_eq!(fish.path_line.as_deref(), Some("fish_add_path --append --path $HOME/.t3/bin"));
    assert_eq!(
        fish.path_setup_command().as_deref(),
        Some("mkdir -p ~/.config/fish && printf '\\n%s\\n' 'fish_add_path --append --path $HOME/.t3/bin' >> ~/.config/fish/config.fish")
    );

    // macOS bash is intentionally omitted (`packages/shared/src/shell.ts` `pathSetup`).
    let macos_bash = detect_cli_command_status(
        std::path::Path::new("/Users/me/.t3/bin"),
        true,
        &[("/usr/bin", false)],
        &ShellEnv { platform: "macos", home: Some("/Users/me"), shell: Some("/bin/bash"), ..Default::default() },
    );
    assert_eq!(macos_bash.profile_file, None);
    assert_eq!(macos_bash.path_line, None);
    assert_eq!(macos_bash.path_setup_command(), None);

    // Linux bash uses ~/.bashrc.
    let linux_bash = detect_cli_command_status(
        std::path::Path::new("/home/me/.t3/bin"),
        true,
        &[("/usr/bin", false)],
        &ShellEnv { platform: "linux", home: Some("/home/me"), shell: Some("/bin/bash"), ..Default::default() },
    );
    assert_eq!(linux_bash.profile_file.as_deref(), Some("~/.bashrc"));
    assert_eq!(linux_bash.path_line.as_deref(), Some("export PATH=\"$PATH:$HOME/.t3/bin\""));

    // Exact line matching ignores comments and substring matches.
    assert!(!profile_contains_line("# export PATH=\"$PATH:$HOME/.t3/bin\"\n", "export PATH=\"$PATH:$HOME/.t3/bin\""));
    assert!(!profile_contains_line("export PATH=\"$PATH:$HOME/.t3/bin2\"\n", "export PATH=\"$PATH:$HOME/.t3/bin\""));
    assert!(profile_contains_line("  export PATH=\"$PATH:$HOME/.t3/bin\"  \n", "export PATH=\"$PATH:$HOME/.t3/bin\""));

    let shadowed = detect_cli_command_status(
        std::path::Path::new("/Users/me/.t3/bin"),
        true,
        &[("/opt/homebrew/bin", true), ("/Users/me/.t3/bin", true)],
        &ShellEnv { platform: "linux", home: Some("/Users/me"), shell: Some("/bin/bash"), ..Default::default() },
    );
    assert_eq!(shadowed.shadowed_by.as_deref(), Some("/opt/homebrew/bin/t3"));
    assert!(shadowed.description().contains("runs first in a new terminal"));
}

#[test]
fn welcome_offers_phone_requires_primary_cloud_share_scopes_and_policy() {
    let base = WelcomePhoneOfferInput {
        is_primary: true,
        cloud_configured: true,
        can_share: true,
        managed_policy: None,
    };
    assert!(welcome_offers_phone(base));
    assert!(!welcome_offers_phone(WelcomePhoneOfferInput { is_primary: false, ..base }));
    assert!(!welcome_offers_phone(WelcomePhoneOfferInput { cloud_configured: false, ..base }));
    assert!(!welcome_offers_phone(WelcomePhoneOfferInput { can_share: false, ..base }));

    let no_remote = json!({"organization": "Acme", "limits": {"clients": ["local"]}});
    assert!(!welcome_offers_phone(WelcomePhoneOfferInput { managed_policy: Some(&no_remote), ..base }));

    let no_connect = json!({"organization": "Acme", "limits": {"connect": false}});
    assert!(!welcome_offers_phone(WelcomePhoneOfferInput { managed_policy: Some(&no_connect), ..base }));

    let allowed = json!({"organization": "Acme", "limits": {"clients": ["local", "remote"], "connect": true}});
    assert!(welcome_offers_phone(WelcomePhoneOfferInput { managed_policy: Some(&allowed), ..base }));
}

#[test]
fn phone_is_shared_enforces_linked_revoked_user_and_tunnel_rules() {
    use crate::mcp::gate::CloudLinkState;

    let mut link = CloudLinkState {
        linked: true,
        cloud_user_id: Some("user_alice".into()),
        link_revoked: Some(false),
        managed_tunnel_active: Some(true),
        ..Default::default()
    };

    assert!(!phone_is_shared(None, PhoneCloudAccount::UserId("user_alice")));
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::SignedOut));
    assert!(phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_bob")));

    // Older servers omit `managedTunnelActive` (`None`), which counts as shared.
    link.managed_tunnel_active = None;
    assert!(phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));

    // Publish-only link (`managedTunnelActive: false`) is not shared for the phone app.
    link.managed_tunnel_active = Some(false);
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));
    link.managed_tunnel_active = Some(true);

    // Revoked or unlinked computers are not shared.
    link.link_revoked = Some(true);
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));
    link.link_revoked = None;
    link.linked = false;
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));
    link.linked = true;

    // Missing `cloudUserId` does not match a known user or an unlisted native identity.
    link.cloud_user_id = None;
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::UserId("user_alice")));
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::NativeIdentity { in_account_relay_list: None }));

    // Native email identity respects relay discovery membership once listed.
    link.cloud_user_id = Some("user_alice".into());
    assert!(phone_is_shared(Some(&link), PhoneCloudAccount::NativeIdentity { in_account_relay_list: None }));
    assert!(phone_is_shared(Some(&link), PhoneCloudAccount::NativeIdentity { in_account_relay_list: Some(true) }));
    assert!(!phone_is_shared(Some(&link), PhoneCloudAccount::NativeIdentity { in_account_relay_list: Some(false) }));
}

#[test]
fn resolve_phone_connect_state_covers_all_six_states() {
    use crate::mcp::gate::{CloudLinkState, ConnectReachability};

    let account = PhoneCloudAccount::UserId("user_alice");
    let shared_base = CloudLinkState {
        linked: true,
        cloud_user_id: Some("user_alice".into()),
        link_revoked: Some(false),
        managed_tunnel_active: Some(true),
        ..Default::default()
    };

    // 1. Signed in & pending (`link == None`): Checking.
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: None,
            is_pending: true,
            has_error: false,
        }),
        PhoneConnectState::Checking
    );
    // Checking wins over stale error while retrying with no cached link.
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: None,
            is_pending: true,
            has_error: true,
        }),
        PhoneConnectState::Checking
    );

    // 2. Signed in & error: Error.
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: None,
            is_pending: false,
            has_error: true,
        }),
        PhoneConnectState::Error
    );

    // 3. Signed in & shared & reachability == "reachable": Reachable (stays stable during background refresh).
    let reachable_link = CloudLinkState {
        reachability: Some(ConnectReachability::Reachable {
            checked_at: "2026-11-20T00:00:00Z".into(),
            ready_connections: Some(1),
            protocol: Some("quic".into()),
        }),
        ..shared_base.clone()
    };
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: Some(&reachable_link),
            is_pending: true,
            has_error: false,
        }),
        PhoneConnectState::Reachable
    );

    // 4. Signed in & shared & reachability == None: SharedUnknownReachability.
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: Some(&shared_base),
            is_pending: false,
            has_error: false,
        }),
        PhoneConnectState::SharedUnknownReachability
    );

    // 5a. Signed in & shared & reachability == "unreachable": SharedUnreachable.
    let unreachable_link = CloudLinkState {
        reachability: Some(ConnectReachability::Unreachable {
            checked_at: "2026-11-20T00:00:00Z".into(),
            cause: "tunnel_down".into(),
            reason: "Tunnel disconnected".into(),
            hint: None,
            fix: "Check T3 Connect".into(),
        }),
        ..shared_base.clone()
    };
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: Some(&unreachable_link),
            is_pending: false,
            has_error: false,
        }),
        PhoneConnectState::SharedUnreachable
    );

    // 5b. Signed in & not shared (unlinked, wrong account, revoked, publish-only, or no link yet): NotShared.
    let other_account_link = CloudLinkState {
        cloud_user_id: Some("user_other".into()),
        reachability: reachable_link.reachability.clone(),
        ..shared_base.clone()
    };
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: Some(&other_account_link),
            is_pending: false,
            has_error: false,
        }),
        PhoneConnectState::NotShared
    );
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account,
            link: None,
            is_pending: false,
            has_error: false,
        }),
        PhoneConnectState::NotShared
    );

    // 6. Signed out: SignedOut regardless of link/pending/error.
    assert_eq!(
        resolve_phone_connect_state(&PhoneConnectInput {
            account: PhoneCloudAccount::SignedOut,
            link: Some(&reachable_link),
            is_pending: true,
            has_error: true,
        }),
        PhoneConnectState::SignedOut
    );
}

#[test]
fn phone_account_for_identity_matches_user_ids_and_relay_discovery() {
    use crate::relay::{Discovery, Endpoint, Environment};

    let empty = Discovery::default();
    assert_eq!(phone_account_for_identity(None, &empty, &["env-1"]), PhoneCloudAccount::SignedOut);
    assert_eq!(phone_account_for_identity(Some("   "), &empty, &["env-1"]), PhoneCloudAccount::SignedOut);
    assert_eq!(
        phone_account_for_identity(Some("user_123"), &empty, &["env-1"]),
        PhoneCloudAccount::UserId("user_123")
    );
    assert_eq!(
        phone_account_for_identity(Some("dev@example.test"), &empty, &["env-1"]),
        PhoneCloudAccount::NativeIdentity { in_account_relay_list: None }
    );

    let listed = Discovery {
        listed: true,
        refreshing: false,
        environments: vec![Environment {
            environment_id: "env-primary".into(),
            label: "Mac".into(),
            endpoint: Endpoint {
                http_base_url: "https://mac.t3.codes".into(),
                ws_base_url: "wss://mac.t3.codes".into(),
                provider_kind: "cloudflare_tunnel".into(),
            },
        }],
    };
    assert_eq!(
        phone_account_for_identity(Some("dev@example.test"), &listed, &["env-primary"]),
        PhoneCloudAccount::NativeIdentity { in_account_relay_list: Some(true) }
    );
    assert_eq!(
        phone_account_for_identity(Some("dev@example.test"), &listed, &["env-other"]),
        PhoneCloudAccount::NativeIdentity { in_account_relay_list: Some(false) }
    );
}
