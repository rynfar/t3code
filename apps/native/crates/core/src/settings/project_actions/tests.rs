use serde_json::json;

use super::*;
use crate::keybindings::decode_resolved_keybindings;

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).expect("settings decode")
}

fn script(id: &str) -> ProjectScript {
    ProjectScript { id: id.into(), name: id.to_uppercase(), command: format!("run {id}"), ..Default::default() }
}

fn member(id: &str) -> Member {
    Member { id: id.into(), scripts: Vec::new() }
}

fn keybindings(rules: Value) -> Vec<ResolvedKeybindingRule> {
    decode_resolved_keybindings(&rules)
}

fn binding(command: &str, key: &str, modifiers: &[&str]) -> Value {
    let held = |name: &str| modifiers.contains(&name);
    json!({"command": command, "shortcut": {"key": key, "modKey": held("mod"), "ctrlKey": held("ctrl"), "metaKey": held("meta"), "altKey": held("alt"), "shiftKey": held("shift")}})
}

#[test]
fn script_ids_come_from_names_and_skip_taken_ones() {
    assert_eq!(next_project_script_id("Dev Server!", [], 0), "dev-server");
    assert_eq!(next_project_script_id("  ", [], 0), "script");
    assert_eq!(next_project_script_id("Dev", ["dev", "dev-2"], 0), "dev-3");
    assert_eq!(next_project_script_id("A very long action name for testing", [], 0), "a-very-long-action-name");
    let long = next_project_script_id("A very long action name for testing", ["a-very-long-action-name"], 0);
    assert_eq!(long, "a-very-long-action-nam-2");
    assert!(long.len() <= 24);
}

#[test]
fn scripts_build_from_the_form_and_release_claimed_roles() {
    let input = ScriptInput {
        name: "Setup".into(),
        command: "bun i".into(),
        run_on_worktree_create: true,
        wait_for_setup: true,
        preview_url: Some("http://localhost:5173".into()),
        auto_open_preview: true,
        ..Default::default()
    };
    let built = build_project_script("setup", &input);
    assert_eq!(
        serde_json::to_value(&built).unwrap(),
        json!({"id": "setup", "name": "Setup", "command": "bun i", "icon": "play", "runOnWorktreeCreate": true, "async": false, "previewUrl": "http://localhost:5173", "autoOpenPreview": true})
    );
    let plain = build_project_script("dev", &ScriptInput { name: "Dev".into(), command: "bun dev".into(), wait_for_setup: true, ..Default::default() });
    assert_eq!(serde_json::to_value(&plain).unwrap(), json!({"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false}));

    let mut other = script("old");
    other.run_on_worktree_create = true;
    other.run_on_settle = Some(true);
    let released = release_claimed_roles(&other, &input);
    assert!(!released.run_on_worktree_create);
    assert_eq!(released.run_on_settle, Some(true), "only the roles the saved action takes move");

    let current = vec![other, script("dev")];
    let saved = submitted_scripts(&current, "setup", &input, true);
    assert_eq!(saved.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["old", "dev", "setup"]);
    assert!(!saved[0].run_on_worktree_create);
}

#[test]
fn the_editor_round_trips_a_script_and_its_shortcut() {
    let mut saved = script("test");
    saved.run_on_worktree_create = true;
    saved.run_async = Some(false);
    let bindings = keybindings(json!([binding("script.test.run", "t", &["mod", "shift"]), binding("script.other.run", "o", &["mod"])]));
    let request = EditorRequest::edit(&saved, &bindings);
    assert_eq!(request.initial.keybinding, Some(Some("mod+shift+t".into())));
    assert!(request.initial.wait_for_setup);
    assert_eq!(keybinding_value_for_command(&bindings, None), None);
    let escape = keybindings(json!([binding("script.go.run", "escape", &["ctrl"])]));
    assert_eq!(keybinding_value_for_command(&escape, Some("script.go.run")).as_deref(), Some("ctrl+esc"));
}

#[test]
fn the_form_validates_like_the_web_editor() {
    let add = EditorRequest::add();
    let form = |name: &str, command: &str, keybinding: &str| FormValues { name: name.into(), command: command.into(), keybinding: keybinding.into(), ..Default::default() };
    let check = |request: &EditorRequest, form: &FormValues, can_edit: bool, can_write: bool| validate_form(request, form, &[script("dev")], can_edit, can_write, 0);
    assert_eq!(check(&add, &form("Dev", "bun dev", ""), false, true).unwrap_err(), "This connection cannot change project actions.");
    assert_eq!(check(&add, &form("Dev", "bun dev", "mod+d"), true, false).unwrap_err(), "This connection cannot change keyboard shortcuts.");
    assert_eq!(check(&add, &form(" ", "bun dev", ""), true, true).unwrap_err(), "Name is required.");
    assert_eq!(check(&add, &form("Dev", " ", ""), true, true).unwrap_err(), "Command is required.");
    assert_eq!(check(&add, &form("Dev", "bun dev", &"x".repeat(65)), true, true).unwrap_err(), INVALID_KEYBINDING);

    let new = check(&add, &form(" Dev ", " bun dev ", " mod+d "), true, true).unwrap();
    assert_eq!((new.name.as_str(), new.command.as_str(), new.keybinding.clone()), ("Dev", "bun dev", Some(Some("mod+d".into()))));
    let unbound = check(&add, &form("Dev", "bun dev", ""), true, false).unwrap();
    assert_eq!(unbound.keybinding, None, "without settings access a new action leaves shortcuts alone");

    let mut edit = EditorRequest::edit(&script("dev"), &[]);
    edit.initial.keybinding = Some(Some("mod+d".into()));
    assert_eq!(check(&edit, &form("Dev", "bun dev", "mod+d"), true, false).unwrap().keybinding, None, "an unchanged shortcut is kept");
    assert_eq!(check(&edit, &form("Dev", "bun dev", ""), true, true).unwrap().keybinding, Some(None), "a cleared shortcut is removed");

    let preview = FormValues { preview_url: "  ".into(), auto_open_preview: true, ..form("Dev", "bun dev", "") };
    let saved = check(&add, &preview, true, true).unwrap();
    assert_eq!((saved.preview_url, saved.auto_open_preview), (None, false));
}

#[test]
fn the_shown_list_is_the_override_else_the_environment_defaults() {
    let settings = settings(json!({
        "defaultProjectScripts": [{"id": "env", "name": "Env", "command": "env", "icon": "play", "runOnWorktreeCreate": false}],
        "projectSettingsOverrides": {"p1": {"defaultProjectScripts": []}, "p2": {"defaultThreadEnvMode": "local"}}
    }));
    assert!(shown_scripts(&settings, "p1").is_empty());
    assert_eq!(shown_scripts(&settings, "p2")[0].id, "env");
    assert!(scripts_mixed(&settings, &["p1", "p2"]));
    assert!(!scripts_mixed(&settings, &["p2", "p3"]));
    assert!(scripts_overridden(&settings, &["p2", "p1"]));
    assert!(!scripts_overridden(&settings, &["p2"]));
}

#[test]
fn saving_writes_the_override_then_the_shortcut() {
    let settings = settings(json!({
        "projectSettingsFolded": true,
        "projectSettingsOverrides": {"p1": {"defaultProjectScripts": [], "branchNamePrefix": "x/"}}
    }));
    let input = ScriptInput { name: "Dev".into(), command: "bun dev".into(), keybinding: Some(Some("mod+d".into())), ..Default::default() };
    let id = submit_id(&settings, &[], &[member("p1")], None, &input.name, 0);
    assert_eq!(id, "dev");
    let writes = plan_writes(&settings, &[], &[member("p1")], &[member("p1")], &ScriptsEdit::Save { id: id.clone(), input, is_new: true });
    assert_eq!(
        writes,
        vec![
            ActionWrite::Settings(json!({"patch": {"projectSettingsOverrides": {"p1": {
                "branchNamePrefix": "x/",
                "defaultProjectScripts": [{"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false}]
            }}}})),
            ActionWrite::UpsertKeybinding(json!({"key": "mod+d", "command": "script.dev.run"})),
        ]
    );
    assert_eq!(writes[1].method(), "server.upsertKeybinding");
}

#[test]
fn a_changed_shortcut_replaces_the_old_one() {
    let settings = settings(json!({"projectSettingsFolded": true, "projectSettingsOverrides": {"p1": {"defaultProjectScripts": [{"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false}]}}}));
    let bindings = keybindings(json!([binding("script.dev.run", "d", &["mod"])]));
    let dev = shown_scripts(&settings, "p1")[0].clone();
    let rebind = ScriptInput { name: dev.name.clone(), command: dev.command.clone(), keybinding: Some(Some("mod+shift+d".into())), ..Default::default() };
    let writes = plan_writes(&settings, &bindings, &[member("p1")], &[member("p1")], &ScriptsEdit::Save { id: "dev".into(), input: rebind.clone(), is_new: false });
    assert_eq!(writes[1], ActionWrite::UpsertKeybinding(json!({"key": "mod+shift+d", "command": "script.dev.run", "replace": {"key": "mod+d", "command": "script.dev.run"}})));
    let unchanged = ScriptInput { keybinding: None, ..rebind };
    let kept = plan_writes(&settings, &bindings, &[member("p1")], &[member("p1")], &ScriptsEdit::Save { id: "dev".into(), input: unchanged, is_new: false });
    assert_eq!(kept.len(), 1, "a save that leaves the shortcut alone sends only the list");
}

#[test]
fn deleting_an_action_keeps_a_shortcut_another_project_still_uses() {
    let dev = json!([{"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false}]);
    let shared = settings(json!({"projectSettingsFolded": true, "projectSettingsOverrides": {"p1": {"defaultProjectScripts": dev}, "p2": {"defaultProjectScripts": dev}}}));
    let bindings = keybindings(json!([binding("script.dev.run", "d", &["mod"])]));
    let remove = ScriptsEdit::Delete { id: "dev".into() };
    let projects = [member("p1"), member("p2")];
    let writes = plan_writes(&shared, &bindings, &projects, &[member("p1")], &remove);
    assert_eq!(writes.len(), 1, "p2 still runs dev, so its shortcut stays");
    let alone = settings(json!({"projectSettingsFolded": true, "projectSettingsOverrides": {"p1": {"defaultProjectScripts": dev}}}));
    let writes = plan_writes(&alone, &bindings, &projects, &[member("p1")], &remove);
    assert_eq!(writes[0], ActionWrite::Settings(json!({"patch": {"projectSettingsOverrides": {"p1": {"defaultProjectScripts": []}}}})));
    assert_eq!(writes[1], ActionWrite::RemoveKeybinding(json!({"key": "mod+d", "command": "script.dev.run"})));
}

#[test]
fn resetting_the_override_clears_it_and_drops_orphaned_shortcuts() {
    let settings = settings(json!({
        "projectSettingsFolded": true,
        "defaultProjectScripts": [{"id": "lint", "name": "Lint", "command": "bun lint", "icon": "lint", "runOnWorktreeCreate": false}],
        "projectSettingsOverrides": {"p1": {"defaultProjectScripts": [{"id": "dev", "name": "Dev", "command": "bun dev", "icon": "play", "runOnWorktreeCreate": false}]}}
    }));
    let bindings = keybindings(json!([binding("script.dev.run", "d", &["mod"])]));
    let writes = plan_writes(&settings, &bindings, &[member("p1")], &[member("p1")], &ScriptsEdit::Reset);
    assert_eq!(
        writes,
        vec![
            ActionWrite::Settings(json!({"patch": {"projectSettingsOverrides": {"p1": null}}})),
            ActionWrite::RemoveKeybinding(json!({"key": "mod+d", "command": "script.dev.run"})),
        ]
    );
}

#[test]
fn t3_json_parses_leniently_and_rejects_invalid_files() {
    let file = parse_project_file(
        r#"{
            // shared actions
            "$schema": "https://t3.codes/schema/t3.json",
            "defaultThreadEnvMode": "worktree",
            "scripts": [
                { "name": " Dev ", "command": "bun dev", "previewUrl": "http://localhost:5173", "autoOpenPreview": true, },
                /* setup */ { "name": "Install", "command": "bun i // not a comment", "icon": "build", "runOnWorktreeCreate": true, "async": false },
            ],
            "unknownKey": 1,
        }"#,
    );
    let ProjectFile::Valid { scripts, default_thread_env_mode } = &file else { panic!("expected a valid file, got {file:?}") };
    assert_eq!(*default_thread_env_mode, Some(crate::server_config::ThreadEnvMode::Worktree));
    assert_eq!(scripts.len(), 2);
    assert_eq!(scripts[0].name, "Dev");
    assert_eq!(scripts[1].command, "bun i // not a comment");
    let dev = scripts[0].import_input();
    assert_eq!((dev.icon, dev.auto_open_preview, dev.keybinding.clone()), (ScriptIcon::Play, true, Some(None)));
    let install = scripts[1].import_input();
    assert_eq!((install.icon, install.run_on_worktree_create, install.wait_for_setup), (ScriptIcon::Build, true, true));

    assert_eq!(parse_project_file("{ not json"), ProjectFile::Invalid);
    assert_eq!(parse_project_file(r#"{"scripts": [{"name": "x", "command": "y", "icon": "rocket"}]}"#), ProjectFile::Invalid);
    assert_eq!(parse_project_file(r#"{"scripts": [{"name": "", "command": "y"}]}"#), ProjectFile::Invalid);
    assert_eq!(parse_project_file(r#"{"defaultThreadEnvMode": "elsewhere"}"#), ProjectFile::Invalid);
    assert_eq!(parse_project_file("[]"), ProjectFile::Invalid);
    assert_eq!(project_file_from_read(&json!({"exists": false})), ProjectFile::Missing);
    assert_eq!(project_file_from_read(&json!({"contents": "{}", "exists": true})), ProjectFile::Valid { scripts: Vec::new(), default_thread_env_mode: None });

    let mut taken = script("dev");
    taken.command = "bun dev".into();
    let mut other = script("x");
    other.name = "install".into();
    assert!(importable(scripts, &[taken, other]).is_empty(), "same command or same name (any case) is already imported");
    assert_eq!(importable(scripts, &[]).len(), 2);
}

#[test]
fn grouped_members_send_each_shortcut_once() {
    let settings = settings(json!({"projectSettingsFolded": true}));
    let input = ScriptInput { name: "Dev".into(), command: "bun dev".into(), keybinding: Some(Some("mod+d".into())), ..Default::default() };
    let writes = plan_writes(&settings, &[], &[member("p1"), member("p2")], &[member("p1"), member("p2")], &ScriptsEdit::Save { id: "dev".into(), input, is_new: true });
    assert_eq!(writes.len(), 2, "one list write for both members, then one shortcut: {writes:?}");
    assert_eq!(writes[0].payload()["patch"]["projectSettingsOverrides"].as_object().map(|entries| entries.len()), Some(2));
}
