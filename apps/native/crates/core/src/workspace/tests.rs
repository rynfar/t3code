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

#[test]
fn previous_worktree_uses_the_latest_eligible_project_thread_and_keeps_first_ties() {
    let threads = [
        (
            "p1",
            Some("old"),
            Some("/wt/old"),
            "2026-01-01T00:00:00Z",
            None,
        ),
        (
            "p1",
            Some("feature"),
            Some("/wt/feature"),
            "2026-02-01T01:00:00+01:00",
            None,
        ),
        (
            "p1",
            Some("tie"),
            Some("/wt/tie"),
            "2026-02-01T00:00:00Z",
            None,
        ),
        (
            "p2",
            Some("other"),
            Some("/wt/other"),
            "2026-03-01T00:00:00Z",
            None,
        ),
        (
            "p1",
            Some("current"),
            Some("/wt/current"),
            "2026-04-01T00:00:00Z",
            None,
        ),
        (
            "p1",
            Some("archived"),
            Some("/wt/archived"),
            "2026-05-01T00:00:00Z",
            Some("archived"),
        ),
        ("p1", Some("invalid"), Some("/wt/invalid"), "invalid", None),
        ("p1", None, Some(""), "2026-06-01T00:00:00Z", None),
    ];
    let select = |current| {
        previous_worktree_seed(
            threads.iter().map(
                |(project, branch, path, updated, archived)| PreviousThread {
                    project_id: project,
                    branch: *branch,
                    worktree_path: *path,
                    updated_at: updated,
                    archived_at: *archived,
                },
            ),
            "p1",
            current,
        )
    };
    let seed = select(Some("/wt/current")).unwrap();
    assert_eq!(seed.branch.as_deref(), Some("feature"));
    assert_eq!(seed.worktree_path, "/wt/feature");
    assert_eq!(seed.label(), "Previous worktree (feature)");
    assert_eq!(select(None).unwrap().branch.as_deref(), Some("current"));
    let seed = previous_worktree_seed(
        [PreviousThread {
            project_id: "p1",
            branch: None,
            worktree_path: Some("/wt/detached"),
            updated_at: "2026-01-01T00:00:00Z",
            archived_at: None,
        }],
        "p1",
        None,
    )
    .unwrap();
    assert_eq!(seed.label(), "Previous worktree");
    assert!(previous_worktree_seed(std::iter::empty(), "p1", None).is_none());
}

#[test]
fn reused_worktree_launches_preserve_scope_branch_and_path_without_checkout() {
    let choice = DraftWorkspace {
        environment_id: "env-a".into(),
        project_root: "/repo".into(),
        mode: ThreadEnvMode::Worktree,
        previous: Some(PreviousWorktreeSeed {
            branch: Some("feature".into()),
            worktree_path: "/wt/feature".into(),
        }),
    };
    assert!(choice.in_scope("env-a", "/repo"));
    assert!(!choice.in_scope("env-b", "/repo"));
    assert!(!choice.in_scope("env-a", "/other"));
    assert_eq!(
        choice.strategy().unwrap().to_json(),
        json!({"type":"existing_worktree", "worktreePath":"/wt/feature", "branch":"feature"})
    );
    let current = DraftWorkspace { mode: ThreadEnvMode::Local, ..choice.clone() };
    assert_eq!(current.strategy(), choice.strategy());
    let detached = DraftWorkspace {
        previous: Some(PreviousWorktreeSeed {
            branch: None,
            worktree_path: "/wt/detached".into(),
        }),
        ..choice.clone()
    };
    assert_eq!(
        detached.strategy().unwrap().to_json(),
        json!({"type":"existing_worktree", "worktreePath":"/wt/detached"})
    );
    let local = DraftWorkspace {
        mode: ThreadEnvMode::Local,
        previous: None,
        ..choice.clone()
    };
    assert_eq!(local.strategy(), Some(Strategy::Root));
    let cwd = draft_cwd(Some(&choice), "env-a", "/repo");
    assert_eq!(crate::composer::menu::search_entries_request(cwd, "only-in-worktree")["cwd"], "/wt/feature");
    assert_eq!(crate::composer::menu::refresh_providers_request("fixture-provider", cwd)["cwd"], "/wt/feature");
    assert_eq!(draft_cwd(Some(&choice), "env-b", "/repo"), "/repo");
    assert_eq!(draft_cwd(Some(&choice), "env-a", "/moved"), "/moved");
    assert_eq!(draft_cwd(Some(&local), "env-a", "/repo"), "/repo");

    let new = DraftWorkspace {
        previous: None,
        ..choice
    };
    assert_eq!(new.strategy(), None);
    assert!(can_use_previous_worktree(false, false, false));
    for (server, locked, force) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        assert!(!can_use_previous_worktree(server, locked, force));
    }
}
