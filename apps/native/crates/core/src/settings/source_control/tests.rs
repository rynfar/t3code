use serde_json::json;

use super::*;
use crate::server_config::ServerSettings;

fn decode(value: Value) -> SourceControlSettings {
    full(value).source_control
}

fn full(value: Value) -> ServerSettings {
    serde_json::from_value(value).unwrap()
}

#[test]
fn source_control_settings_take_contract_defaults_and_legacy_styles() {
    let settings = decode(json!({}));
    assert_eq!(settings, SourceControlSettings::default());
    assert!(settings.source_control_writing_style.follow_change_request_templates);
    assert!(settings.source_control_writing_style.is_default_style());

    let legacy = decode(json!({ "sourceControlWritingStyle": "  Short titles. " }));
    assert_eq!(legacy.source_control_writing_style.mode, WritingMode::Custom);
    assert_eq!(legacy.source_control_writing_style.custom_instructions, "Short titles.");
    assert_eq!(decode(json!({ "sourceControlWritingStyle": "" })).source_control_writing_style, WritingStyle::default());

    let all = full(json!({
        "defaultAutoPull": true,
        "removeAgentCreditsOnMerge": "yes",
        "sourceControlWritingStyle": { "mode": "conventional_commits", "followChangeRequestTemplates": false },
        "reviewGuideModelSelection": { "instanceId": "codex", "model": "gpt-5" },
        "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-5-mini" },
    }));
    let settings = &all.source_control;
    assert!(settings.default_auto_pull);
    assert!(!settings.remove_agent_credits_on_merge);
    assert_eq!(settings.source_control_writing_style.mode, WritingMode::ConventionalCommits);
    assert!(!settings.source_control_writing_style.follow_change_request_templates);
    assert_eq!(writer_model(&all).map(|selection| selection.model), Some("gpt-5-mini".to_owned()));
}

#[test]
fn source_control_changes_patch_only_what_they_change() {
    let payload = |change: SourceControlChange| change.into_server().payload()["patch"].clone();
    assert_eq!(payload(SourceControlChange::MergeMethod(None)), json!({ "pullRequestMergeMethod": null }));
    assert_eq!(parse_merge_method("squash"), Some(Some(MergeMethod::Squash)));
    assert_eq!(parse_merge_method("fast-forward"), Some(Some(MergeMethod::FastForward)));
    assert_eq!(merge_method_value(None), "last");
    assert_eq!(
        payload(SourceControlChange::WritingMode { mode: WritingMode::Custom, custom_instructions: Some(" Be brief ".into()) }),
        json!({ "sourceControlWritingStyle": { "mode": "custom", "customInstructions": "Be brief" } })
    );
    assert_eq!(
        payload(SourceControlChange::WritingMode { mode: WritingMode::RepoConventions, custom_instructions: None }),
        json!({ "sourceControlWritingStyle": { "mode": "repo_conventions" } })
    );
    assert_eq!(
        payload(SourceControlChange::ResetWritingStyle),
        json!({ "sourceControlWritingStyle": { "mode": "repo_conventions", "customInstructions": "" } }),
        "reset leaves followChangeRequestTemplates alone"
    );
    assert_eq!(payload(SourceControlChange::FollowTemplates(false)), json!({ "sourceControlWritingStyle": { "followChangeRequestTemplates": false } }));
    assert_eq!(payload(SourceControlChange::WriterModel(None)), json!({ "sourceControlWriterModelSelection": null }));
}

#[test]
fn discovery_decodes_effect_options_and_summarizes_like_the_web() {
    let discovery = decode_discovery(&json!({
        "versionControlSystems": [
            { "kind": "git", "label": "Git", "executable": "git", "status": "available", "version": { "_tag": "Some", "value": "2.45.0" }, "installHint": "Install Git", "detail": { "_tag": "None" }, "implemented": true },
            { "kind": "jj", "label": "Jujutsu", "status": "missing", "version": { "_tag": "None" }, "installHint": "Install jj", "implemented": false },
            { "label": "broken" },
        ],
        "sourceControlProviders": [
            { "kind": "github", "label": "GitHub", "executable": "gh", "status": "available", "version": { "_tag": "None" }, "installHint": "Install gh",
              "auth": { "status": "authenticated", "account": { "_tag": "Some", "value": "octocat" }, "host": { "_tag": "None" }, "detail": { "_tag": "Some", "value": "github.com" } } },
            { "kind": "gitlab", "label": "GitLab", "executable": "glab", "status": "available", "version": { "_tag": "None" }, "installHint": "Install glab",
              "auth": { "status": "unauthenticated", "account": { "_tag": "None" }, "host": { "_tag": "None" }, "detail": { "_tag": "None" } } },
            { "kind": "bitbucket", "label": "Bitbucket", "status": "available", "version": { "_tag": "None" }, "installHint": "Add an API token.",
              "auth": { "status": "unauthenticated", "account": { "_tag": "None" }, "host": { "_tag": "None" }, "detail": { "_tag": "None" } } },
        ],
    }));
    let [git, jj] = &discovery.version_control_systems[..] else { panic!("the undecodable item is skipped alone") };
    assert_eq!(git.version.as_deref(), Some("2.45.0"));
    assert_eq!((git.dot(), git.enabled(), git.summary().as_str()), (Dot::Success, true, "Available"));
    assert!(jj.coming_soon());
    assert_eq!((jj.dot(), jj.summary().as_str()), (Dot::Muted, "Support for Jujutsu is coming soon."));

    let [github, gitlab, bitbucket] = &discovery.source_control_providers[..] else { panic!() };
    assert_eq!(github.auth.as_ref().and_then(|auth| auth.account.as_deref()), Some("octocat"));
    assert_eq!((github.dot(), github.summary().as_str(), github.auth_badge()), (Dot::Success, "Authenticated \u{00b7} github.com", None));
    assert_eq!(gitlab.auth_badge(), Some("Not authenticated"));
    assert_eq!(gitlab.summary(), "GitLab is not authenticated on this server. Sign in or configure credentials using the glab tool on the server host to enable change request features.");
    assert_eq!(bitbucket.summary(), "Available. Add an API token.");
}

#[test]
fn saving_or_removing_one_host_token_preserves_other_hosts_tokens_and_references() {
    let settings = full(json!({
        "offeredSourceControlProviders": { "github": true, "gitcafe": false, "bad": "ignored" },
        "sourceControlHosts": {
            "github": {
                "tokens": {
                    "github.com": "__REDACTED_SECRET__",
                    "ghe.corp.internal": { "provider": "op", "reference": "op://Work/GHE/token" }
                },
                "signIns": {
                    "github.com": "__REDACTED_SESSION__",
                    "ghe.corp.internal": "__REDACTED_GHE_SESSION__"
                },
                "hosts": {
                    "ghe.corp.internal": { "enabled": false, "account": "corp-user" }
                }
            }
        }
    }));
    assert!(settings.source_control.is_provider_offered("github"));
    assert!(!settings.source_control.is_provider_offered("gitcafe"));
    assert!(settings.source_control.is_provider_offered("bitbucket"));

    let creds = read_host_credential_settings(&settings.source_control_hosts, "github");
    assert_eq!(creds.tokens.len(), 2);
    assert_eq!(creds.tokens.get("github.com"), Some(&SecretValue::Literal("__REDACTED_SECRET__".into())));
    assert_eq!(
        creds.tokens.get("ghe.corp.internal"),
        Some(&SecretValue::Reference { provider: "op".into(), reference: "op://Work/GHE/token".into() })
    );

    // Saving a new host token sends the whole `tokens` map so other hosts' tokens are preserved (`pingdotgg/t3code#29554`).
    let save_patch = SourceControlChange::SaveHostToken {
        kind: "github".into(),
        existing: creds.tokens.clone(),
        host: " GHE.Staging.Corp ".into(),
        token: SecretValue::Literal("ghp_new".into()),
    }
    .into_server()
    .payload()["patch"]
        .clone();
    assert_eq!(
        save_patch,
        json!({
            "sourceControlHosts": {
                "github": {
                    "tokens": {
                        "github.com": "__REDACTED_SECRET__",
                        "ghe.corp.internal": { "provider": "op", "reference": "op://Work/GHE/token" },
                        "ghe.staging.corp": "ghp_new"
                    }
                }
            }
        })
    );

    // Removing one host token clears only that host while keeping the other hosts intact.
    let remove_patch = SourceControlChange::SaveHostToken {
        kind: "github".into(),
        existing: creds.tokens,
        host: "github.com".into(),
        token: SecretValue::Literal(String::new()),
    }
    .into_server()
    .payload()["patch"]
        .clone();
    assert_eq!(
        remove_patch,
        json!({
            "sourceControlHosts": {
                "github": {
                    "tokens": {
                        "github.com": "",
                        "ghe.corp.internal": { "provider": "op", "reference": "op://Work/GHE/token" }
                    }
                }
            }
        })
    );

    // Signing out of one host preserves other hosts' sign-in sessions.
    let sign_out_patch = SourceControlChange::SignOutApp {
        kind: "github".into(),
        existing: creds.sign_ins,
        host: "github.com".into(),
    }
    .into_server()
    .payload()["patch"]
        .clone();
    assert_eq!(
        sign_out_patch,
        json!({
            "sourceControlHosts": {
                "github": {
                    "signIns": {
                        "github.com": "",
                        "ghe.corp.internal": "__REDACTED_GHE_SESSION__"
                    }
                }
            }
        })
    );
}

#[test]
fn accounts_grouping_host_choices_and_sign_in_prompt_follow_web() {
    let prompt = decode_sign_in_prompt(&json!({
        "userCode": "ABCD-1234",
        "verificationUri": "https://github.com/login/device",
        "expiresAt": 1700000000000_i64
    }))
    .unwrap();
    assert_eq!(prompt.user_code, "ABCD-1234");
    assert_eq!(prompt.verification_uri, "https://github.com/login/device");

    let discovery = decode_discovery(&json!({
        "versionControlSystems": [],
        "sourceControlProviders": [{
            "kind": "github",
            "label": "GitHub",
            "executable": "gh",
            "status": "available",
            "installHint": "",
            "appSignIn": true,
            "auth": {
                "status": "authenticated",
                "account": { "_tag": "Some", "value": "alice" },
                "accounts": [
                    { "host": "github.com", "account": "alice", "authenticated": true, "active": true },
                    { "host": "github.com", "account": "bob", "authenticated": true, "active": false },
                    { "host": "github.com", "account": "expired", "authenticated": false, "active": false, "detail": { "_tag": "Some", "value": "Token expired" } },
                    { "host": "ghe.local", "account": "ci-bot", "authenticated": true, "active": true, "environmentVariable": { "_tag": "Some", "value": "GH_TOKEN" } }
                ]
            }
        }]
    }));
    let gh = &discovery.source_control_providers[0];
    assert!(gh.app_sign_in);
    let groups = group_host_accounts(&gh.auth.as_ref().unwrap().accounts);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].host, "github.com");
    assert_eq!(groups[0].active_account.as_deref(), Some("alice"));
    assert_eq!(groups[0].selectable, vec!["alice", "bob"]);
    assert_eq!(groups[0].broken.len(), 1);
    assert_eq!(groups[0].broken[0].account, "expired");
    assert_eq!(groups[1].host, "ghe.local");
    assert_eq!(groups[1].environment_variable.as_deref(), Some("GH_TOKEN"));

    let mut choices = BTreeMap::new();
    choices = next_host_account_choices(&choices, "github.com", None, Some(Some("bob".into())));
    assert_eq!(choices["github.com"], HostAccountChoice { enabled: true, account: Some("bob".into()) });
    // Clearing account while enabled omits the host entry (returns to default).
    choices = next_host_account_choices(&choices, "github.com", None, Some(None));
    assert!(choices.is_empty());
    // Disabling keeps the entry.
    choices = next_host_account_choices(&choices, "github.com", Some(false), None);
    assert_eq!(choices["github.com"], HostAccountChoice { enabled: false, account: None });
    assert_eq!(
        SourceControlChange::SaveHostAccounts { kind: "github".into(), hosts: choices }.into_server().payload()["patch"],
        json!({ "sourceControlHosts": { "github": { "hosts": { "github.com": { "enabled": false } } } } })
    );
}

#[test]
fn issue_tracker_summary_describes_disconnected_rejected_and_linked_states() {
    use crate::issues::wire::{
        IssueTrackerConnection, IssueTrackerKind, IssueTrackerProjectBinding,
        StandaloneIssueTrackerCredential,
    };
    use std::collections::HashMap;

    let mut tracker = IssueTrackerConnection {
        tracker: IssueTrackerKind::Linear,
        display_name: "Linear".into(),
        host: "linear.app".into(),
        scope_noun: "team".into(),
        credential: StandaloneIssueTrackerCredential {
            label: "Personal API key".into(),
            help_url: Some("https://linear.app/settings/api".into()),
        },
        connected: false,
        account: None,
        detail: None,
        workspace: None,
        scopes: Vec::new(),
    };
    assert_eq!(
        issue_tracker_summary(&tracker, &HashMap::new()),
        "Not connected. Add a personal api key to list its issues."
    );

    tracker.connected = true;
    assert_eq!(
        issue_tracker_summary(&tracker, &HashMap::new()),
        "The saved credential was rejected."
    );
    tracker.detail = Some("Token expired".into());
    assert_eq!(issue_tracker_summary(&tracker, &HashMap::new()), "Token expired");

    tracker.account = Some("alex@example.com".into());
    assert_eq!(
        issue_tracker_summary(&tracker, &HashMap::new()),
        "Connected as alex@example.com · no projects linked yet. Pick a team for a project to list its issues."
    );

    let mut bindings = HashMap::new();
    bindings.insert(
        "p1".into(),
        IssueTrackerProjectBinding { tracker: IssueTrackerKind::Linear, scope: "ENG".into(), additional_scopes: Vec::new() },
    );
    assert_eq!(
        issue_tracker_summary(&tracker, &bindings),
        "Connected as alex@example.com · 1 project linked"
    );
    bindings.insert(
        "p2".into(),
        IssueTrackerProjectBinding { tracker: IssueTrackerKind::Linear, scope: "OPS".into(), additional_scopes: Vec::new() },
    );
    assert_eq!(
        issue_tracker_summary(&tracker, &bindings),
        "Connected as alex@example.com · 2 projects linked"
    );
}
