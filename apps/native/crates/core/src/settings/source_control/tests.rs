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
    assert_eq!(
        gitlab.summary(),
        "GitLab is not authenticated on this server. Sign in or configure credentials using the glab tool on the server host to enable change request features."
    );
    assert_eq!(bitbucket.summary(), "Available. Add an API token.");
}
