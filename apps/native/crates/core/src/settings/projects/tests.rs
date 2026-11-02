use serde_json::json;

use super::*;
use crate::server_config::ServerConfig;

fn group(key: &str, label: &str, members: &[&str], archived: bool) -> ProjectGroup {
    ProjectGroup {
        key: key.into(),
        label: label.into(),
        representative_id: members[0].into(),
        member_project_ids: members.iter().map(|id| (*id).to_owned()).collect(),
        pinned: false,
        archived,
    }
}

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).expect("settings decode")
}

#[test]
fn groups_sort_by_name_and_name_twins_show_their_machine() {
    let mut groups = vec![group("b", "beta", &["p2"], false), group("a", "Alpha", &["p1"], true), group("c", "beta", &["p3"], false)];
    sort_groups(&mut groups);
    assert_eq!(groups.iter().map(|g| g.key.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    assert_eq!(choice_secondary(&groups[0], &groups, "/w/alpha", "This Mac"), "Archived \u{b7} /w/alpha");
    assert_eq!(choice_secondary(&groups[1], &groups, "/w/beta", "This Mac"), "This Mac \u{b7} /w/beta");
}

#[test]
fn a_regrouped_project_is_followed_by_its_members() {
    let groups = vec![group("repo:x", "x", &["p1", "p2"], false), group("other", "y", &["p3"], false)];
    assert_eq!(resolve_selection(&groups, "other", &[]).map(|g| g.key.as_str()), Some("other"));
    // Grouping by repository changed the key; the page follows p1 to its new group.
    assert_eq!(resolve_selection(&groups, "local:/w/x", &["p1".into()]).map(|g| g.key.as_str()), Some("repo:x"));
    assert_eq!(resolve_selection(&groups, "gone", &["p9".into()]), None);
}

#[test]
fn project_icons_decode_describe_and_know_their_default() {
    let decode = |value: Value| ProjectIcon::decode(Some(&value));
    assert_eq!(ProjectIcon::decode(None), None);
    assert_eq!(ProjectIcon::decode(Some(&Value::Null)), None);
    let lucide = decode(json!({"kind": "lucide", "name": "rocket", "color": "blue"}));
    assert_eq!(icon_description(lucide.as_ref(), None), "rocket \u{b7} blue");
    let legacy = decode(json!({"kind": "lucide", "name": "rocket", "color": "red", "monogramText": "T3"}));
    assert_eq!(legacy, Some(ProjectIcon::Monogram { text: "T3".into(), color: "red".into() }), "a legacy monogram reads as one");
    assert_eq!(icon_description(decode(json!({"kind": "emoji", "emoji": "🚀"})).as_ref(), None), "🚀");
    let future = decode(json!({"kind": "sticker", "id": 4}));
    assert_eq!(future, Some(ProjectIcon::Unknown));
    assert_eq!(icon_description(future.as_ref(), Some("logo.png")), "logo.png");
    assert_eq!(icon_description(None, None), "Automatic");

    assert!(has_default_icon(false, None, None));
    assert!(!has_default_icon(false, None, future.as_ref()));
    assert!(!has_default_icon(false, Some("logo.png"), None));
    let scratch = decode(json!({"kind": "lucide", "name": "message-square-dashed", "color": "gray"}));
    assert!(has_default_icon(true, None, scratch.as_ref()));
    assert!(!has_default_icon(true, None, None), "a scratch project's default is its dashed chat icon");
    assert_eq!(ProjectUpdate::default_icon(true).payload("c", "p")["projectIcon"], json!({"kind": "lucide", "name": "message-square-dashed", "color": "gray"}));
    let reset = ProjectUpdate::default_icon(false).payload("c", "p");
    assert_eq!((reset["faviconPath"].clone(), reset["projectIcon"].clone()), (Value::Null, Value::Null));
}

#[test]
fn updates_send_only_what_changed_and_removal_forces() {
    let pin = ProjectUpdate { pinned: Some(true), ..Default::default() }.payload("c1", "p1");
    assert_eq!(pin, json!({"type": "project.update", "commandId": "c1", "projectId": "p1", "pinned": true}));
    let rename = ProjectUpdate { title: Some("New".into()), ..Default::default() }.payload("c2", "p1");
    assert_eq!(rename["title"], "New");
    assert!(rename.get("archived").is_none());
    assert_eq!(delete_project("c3", "p1"), json!({"type": "project.delete", "commandId": "c3", "projectId": "p1", "force": true}));
}

#[test]
fn renames_save_only_an_edited_different_title() {
    assert_eq!(rename(&["Old"], "   ", true), Rename::Empty);
    assert_eq!(rename(&["Old"], " New ", true), Rename::Save("New".into()));
    assert_eq!(rename(&["Old"], "New", false), Rename::Unchanged, "blur without typing never saves");
    assert_eq!(rename(&["Same", "Same"], "Same", true), Rename::Unchanged);
    assert_eq!(rename(&["Same", "Other"], "Same", true), Rename::Save("Same".into()), "members that differ converge");
    assert!(can_reset_scratch_title(&[(true, "Scratch")]));
    assert!(!can_reset_scratch_title(&[(true, SCRATCH_PROJECT_TITLE)]));
    assert!(!can_reset_scratch_title(&[(true, "Scratch"), (false, "Repo")]));
    assert!(is_scratch("/home/.t3/scratch/", Some("/home/.t3/scratch")));
    assert!(!is_scratch("/w/x", None));
}

#[test]
fn removal_confirmations_follow_plan_project_removal() {
    let one = RemovalMember { title: "App", workspace_root: "/w/app", environment_label: Some("This Mac") };
    let plan = plan_removal(&[one], 1, false, "App", 2);
    assert!(plan.is_whole_group);
    assert_eq!(
        plan.confirmation,
        "Remove project \"App\" and delete its 2 threads?\nPath: /w/app\nEnvironment: This Mac\nThose threads and any archived threads are deleted from T3 Code. Provider sessions are kept.\nThis removes only the project entries, not the files on disk.\nThis action cannot be undone."
    );
    let checkout = plan_removal(&[one], 3, false, "App", 0);
    assert!(!checkout.is_whole_group);
    assert!(checkout.confirmation.starts_with("Remove checkout \"App\"?\n"));
    assert!(checkout.confirmation.contains("Other entries in this grouped project are unaffected."));
    let other = RemovalMember { title: "App 2", workspace_root: "/w/app2", environment_label: None };
    let all = plan_removal(&[one, other], 2, false, "Grouped", 1);
    assert!(all.confirmation.starts_with("Remove project \"Grouped\" and delete its 1 thread?\nThis removes 2 grouped project entries.\n"));

    let (title, description) = confirm_copy(&plan.confirmation);
    assert_eq!(title, "Remove project \"App\" and delete its 2 threads?");
    assert!(description.is_some_and(|text| text.starts_with("Path: /w/app\n")));
    assert_eq!(confirm_copy("Proceed now"), ("Confirm action".into(), Some("Proceed now".into())));
    assert_eq!(confirm_copy("Sure? It is final."), ("Sure?".into(), Some("It is final.".into())));

    assert_eq!(danger_copy(false, 1).button, "Remove project");
    assert_eq!(danger_copy(false, 3).description, "Deletes all 3 checkout entries and their threads on every machine. Files on disk are not touched.");
    assert_eq!(danger_copy(true, 3).title, "Remove checkout");
}

#[test]
fn override_writes_keep_unknown_keys_and_clear_to_null() {
    let settings = settings(json!({
        "projectSettingsOverrides": {
            "p1": {"defaultThreadEnvMode": "worktree", "branchNamePrefix": "x/", "futureKey": {"a": 1}},
            "p2": {"defaultModelSelection": null}
        }
    }));
    let set = override_set_patch(&settings, &["p1", "p3"], "defaultThreadEnvMode", json!("local"));
    assert_eq!(
        set,
        json!({"patch": {"projectSettingsOverrides": {
            "p1": {"defaultThreadEnvMode": "local", "branchNamePrefix": "x/", "futureKey": {"a": 1}},
            "p3": {"defaultThreadEnvMode": "local"}
        }}})
    );
    let model = override_set_patch(&settings, &["p1"], "defaultModelSelection", Value::Null);
    assert_eq!(model["patch"]["projectSettingsOverrides"]["p1"]["defaultModelSelection"], Value::Null, "null is a stored model override");
    let unset = override_set_patch(&settings, &["p1"], "defaultThreadEnvMode", Value::Null);
    assert!(unset["patch"]["projectSettingsOverrides"]["p1"].get("defaultThreadEnvMode").is_none());
    let cleared = override_clear_patch(&settings, &["p1", "p2", "p4"], &["defaultThreadEnvMode", "defaultModelSelection"]);
    assert_eq!(
        cleared,
        json!({"patch": {"projectSettingsOverrides": {
            "p1": {"branchNamePrefix": "x/", "futureKey": {"a": 1}},
            "p2": null,
            "p4": null
        }}})
    );
}

fn config(value: Value) -> ServerConfig {
    serde_json::from_value(value).expect("config decode")
}

#[test]
fn the_project_model_skips_overrides_on_disabled_instances() {
    let providers = json!([
        {"instanceId": "codex", "driver": "codex", "status": "ready", "enabled": true, "installed": true, "models": [{"slug": "gpt-5", "name": "GPT-5", "isCustom": false}]},
        {"instanceId": "claudeAgent", "driver": "claudeAgent", "status": "ready", "enabled": true, "installed": true, "models": [{"slug": "opus", "name": "Opus", "isCustom": false}]}
    ]);
    let environment_default = json!({"instanceId": "codex", "model": "gpt-5"});
    let overridden = config(json!({"providers": providers, "settings": {
        "defaultModelSelection": environment_default,
        "projectSettingsOverrides": {
            "p1": {"defaultModelSelection": {"instanceId": "claudeAgent", "model": "opus"}},
            "p2": {"defaultModelSelection": {"instanceId": "claudeAgent", "model": "opus"}, "providerInstances": {"claudeAgent": {"enabled": false}}},
            "p3": {"defaultModelSelection": null}
        }
    }}));
    let p1 = project_model(&overridden, "p1");
    assert_eq!((p1.shown.map(|s| s.model), p1.source, p1.automatic), (Some("opus".into()), Source::Project, false));
    let p2 = project_model(&overridden, "p2");
    assert_eq!((p2.shown.map(|s| s.model), p2.source), (Some("gpt-5".into()), Source::Environment), "an override on a disabled instance falls through");
    let p3 = project_model(&overridden, "p3");
    assert_eq!((p3.source, p3.automatic), (Source::Project, true), "a null override picks automatically");
    assert!(p3.shown.is_some());
    let none = project_model(&overridden, "p9");
    assert_eq!((none.source, none.automatic), (Source::Environment, false));
}

#[test]
fn the_project_workspace_falls_back_to_t3_json_then_the_checkout() {
    let settings = settings(json!({"projectSettingsOverrides": {"p1": {"defaultThreadEnvMode": "worktree"}}}));
    assert_eq!(project_env_mode(&settings, "p1", Some(ThreadEnvMode::Local)), (ThreadEnvMode::Worktree, Source::Project));
    assert_eq!(project_env_mode(&settings, "p2", Some(ThreadEnvMode::Worktree)), (ThreadEnvMode::Worktree, Source::ProjectFile));
    assert_eq!(project_env_mode(&settings, "p2", None), (ThreadEnvMode::Local, Source::Environment));
}
