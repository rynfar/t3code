use super::*;

fn settings(value: Value) -> ServerSettings {
    serde_json::from_value(value).unwrap()
}

#[test]
fn env_mode_prefers_the_project_then_the_environment_then_t3_json() {
    let both = settings(json!({"defaultThreadEnvMode": "worktree", "projectSettingsOverrides": {"p1": {"defaultThreadEnvMode": "local"}}}));
    assert_eq!(configured_env_mode(&both, "p1"), Some((ThreadEnvMode::Local, Source::Project)));
    assert_eq!(configured_env_mode(&both, "p2"), Some((ThreadEnvMode::Worktree, Source::Environment)));
    assert_eq!(configured_env_mode(&settings(json!({})), "p1"), None);
    assert_eq!(project_file_env_mode(r#"{"defaultThreadEnvMode": "worktree", "other": 1}"#), Some(ThreadEnvMode::Worktree));
    assert_eq!(project_file_env_mode(r#"{"defaultThreadEnvMode": "cloud"}"#), None);
    assert_eq!(project_file_env_mode("not json"), None);
    assert_eq!(new_thread_env_mode(None, Some(ThreadEnvMode::Worktree), None, None, false), ThreadEnvMode::Worktree);
    assert_eq!(new_thread_env_mode(None, None, None, None, false), ThreadEnvMode::Local);
}

#[test]
fn new_name_only_projects_start_local_until_their_first_thread() {
    let environment = Some((ThreadEnvMode::Worktree, Source::Environment));
    let new_project = Some("/Users/me/T3 Projects/demo");
    let root = Some("/Users/me/T3 Projects/");
    assert_eq!(new_thread_env_mode(environment, None, new_project, root, false), ThreadEnvMode::Local);
    assert_eq!(new_thread_env_mode(environment, None, new_project, root, true), ThreadEnvMode::Worktree);
    assert_eq!(new_thread_env_mode(environment, None, Some("/Users/me/T3 Projects/a/b"), root, false), ThreadEnvMode::Worktree);
    let project = Some((ThreadEnvMode::Worktree, Source::Project));
    assert_eq!(new_thread_env_mode(project, None, new_project, root, false), ThreadEnvMode::Worktree);
    assert_eq!(new_thread_env_mode(environment, None, Some("C:\\T3\\Demo"), Some("c:/t3"), false), ThreadEnvMode::Local);
}

#[test]
fn worktree_launches_start_from_the_default_branch_and_need_one() {
    let refs: VcsRefs = serde_json::from_value(json!({
        "isRepo": true, "hasPrimaryRemote": true, "nextCursor": null, "totalCount": 2,
        "refs": [{"name": "feature", "current": true, "isDefault": false, "worktreePath": null},
                 {"name": "main", "current": false, "isDefault": true, "worktreePath": null}]
    }))
    .unwrap();
    assert_eq!(base_branch(&refs).as_deref(), Some("main"));
    let strategy = launch_strategy(ThreadEnvMode::Worktree, Some(&refs), true).unwrap();
    assert_eq!(strategy.to_json(), json!({"type": "worktree", "baseRef": "main", "startFromOrigin": true}));
    assert_eq!(launch_strategy(ThreadEnvMode::Worktree, Some(&refs), false).unwrap().to_json(), json!({"type": "worktree", "baseRef": "main"}));
    assert_eq!(launch_strategy(ThreadEnvMode::Local, Some(&refs), true), Ok(Strategy::Root));
    let not_repo = VcsRefs { is_repo: false, refs: vec![] };
    assert_eq!(launch_strategy(ThreadEnvMode::Worktree, Some(&not_repo), true), Ok(Strategy::Root));
    let empty = VcsRefs { is_repo: true, refs: vec![] };
    assert!(launch_strategy(ThreadEnvMode::Worktree, Some(&empty), true).is_err());
    assert_eq!(Strategy::Root.to_json(), json!({"type": "root"}));
}

#[test]
fn start_from_origin_follows_the_project_then_the_environment_default() {
    let off = settings(json!({"newWorktreesStartFromOrigin": false, "projectSettingsOverrides": {"p1": {"newWorktreesStartFromOrigin": true}}}));
    assert!(start_from_origin(&off, "p1", ThreadEnvMode::Worktree));
    assert!(!start_from_origin(&off, "p2", ThreadEnvMode::Worktree));
    assert!(start_from_origin(&settings(json!({})), "p2", ThreadEnvMode::Worktree));
    assert!(!start_from_origin(&settings(json!({})), "p2", ThreadEnvMode::Local));
}

#[test]
fn project_file_results_skip_missing_and_truncated_files() {
    assert_eq!(project_file_contents(&json!({"contents": "{}", "truncated": false, "exists": true})), Some("{}"));
    assert_eq!(project_file_contents(&json!({"contents": "", "truncated": false, "exists": false})), None);
    assert_eq!(project_file_contents(&json!({"contents": "{", "truncated": true})), None);
}
