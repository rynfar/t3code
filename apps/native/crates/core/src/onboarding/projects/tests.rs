use super::*;
fn candidate(path: &str) -> Candidate {
    serde_json::from_value(json!({"path":path,"title":path,"sources":["codex"],"threadCount":3,"lastActiveAt":"2026-08-20T12:00:00.000Z"})).unwrap()
}
#[test]
fn web_recent_defaults_include_legacy_and_existing_but_not_folders_thin_old_future_or_invalid() {
    let now = parse_iso("2026-08-22T12:00:00.000Z").unwrap();
    let mut c = candidate("/repo");
    assert!(recent(&c, now));
    c.project_id = Some("existing".into());
    assert!(recent(&c, now));
    c.git = Value::Null;
    assert!(!recent(&c, now));
    c.git = legacy_git();
    c.thread_count = 2;
    assert!(!recent(&c, now));
    c.thread_count = 3;
    for at in ["2026-07-01T12:00:00.000Z", "2026-08-23T12:00:00.000Z", "invalid"] {
        c.last_active_at = Some(at.into());
        assert!(!recent(&c, now));
    }
    c.last_active_at = Some("2026-07-23T12:00:00.000Z".into());
    assert!(recent(&c, now));
    c.last_active_at = None;
    assert!(!recent(&c, now));
}
#[test]
fn web_grouping_preserves_clones_local_legacy_and_other_folders() {
    let mut main = candidate("/main");
    main.git = json!({"remoteKey":"github.com/a/b","repository":"a/b"});
    let mut clone = main.clone();
    clone.path = "/clone".into();
    clone.thread_count = 13;
    let local = candidate("/local");
    let mut folder = candidate("/other");
    folder.git = Value::Null;
    let (g, o) = group(&[main, clone, local, folder]);
    assert_eq!(o, vec![3]);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].label, "/local");
    assert_eq!(g[1].label, "a/b");
    assert_eq!(g[1].candidates, vec![0, 1]);
    assert_eq!(g[1].thread_count, 16);
}
#[test]
fn web_project_matching_prefers_server_id_and_normalizes_only_in_destination() {
    let p: ProjectShell = serde_json::from_value(json!({"id":"local","environmentId":"local","workspaceRoot":"C:\\Work\\Repo\\","title":"repo","createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-01T00:00:00Z"})).unwrap();
    let mut c = candidate("c:/work/repo");
    assert_eq!(resolve_project(&c, "local", "local", std::slice::from_ref(&p)), Some("local".into()));
    assert_eq!(resolve_project(&c, "remote", "local", std::slice::from_ref(&p)), None);
    c.project_id = Some("scanned".into());
    assert_eq!(resolve_project(&c, "local", "local", std::slice::from_ref(&p)), Some("scanned".into()));
    let mut unc = p;
    unc.workspace_root = "\\\\Server\\Share\\Repo\\".into();
    c.path = "\\\\server/SHARE/repo".into();
    c.project_id = None;
    assert_eq!(resolve_project(&c, "local", "local", &[unc]), Some("local".into()));
}
#[test]
fn web_paths_keep_roots_and_split_posix_windows_unc() {
    for root in ["/", "C:\\", "D:/", "\\\\server\\share\\", "//server/share"] {
        assert_eq!(split_path(root), (root, ""));
    }
    assert_eq!(split_path("C:\\Work\\Repo\\"), ("Repo", "C:\\Work\\"));
    assert_eq!(split_path("/code/repo/"), ("repo", "/code/"));
    assert_eq!(split_path("\\\\server\\share\\repo"), ("repo", "\\\\server\\share\\"));
}
#[test]
fn web_partial_success_retry_and_environment_qualified_landing() {
    let (a, b) = (key("a", "/repo"), key("b", "/repo"));
    assert_ne!(a, b);
    let mut p = Progress {
        selection: vec![a.clone(), b.clone()],
        ..Default::default()
    };
    p.record(a.clone(), "partial".into(), ImportResult { imported_count: 2, skipped_count: 1 });
    p.record(b.clone(), "complete".into(), ImportResult { imported_count: 0, skipped_count: 0 });
    p.attempts.insert(a.clone(), "stable-attempt".into());
    p.failed_attempt(&a, false);
    assert_eq!(p.attempts.get(&a).map(String::as_str), Some("stable-attempt"));
    p.failed_attempt(&a, true);
    assert!(!p.attempts.contains_key(&a));
    p.failures = 0;
    assert!(!p.completed.contains_key(&a));
    assert!(p.completed.contains_key(&b));
    assert_eq!(p.landing(), Some("partial"));
    assert_eq!(p.warning().as_deref(), Some("Imported 2 threads. 1 thread could not be imported."));
    p.selection = vec![b];
    assert_eq!(p.landing(), Some("complete"));
    p.selection = vec![key("c", "/repo")];
    assert_eq!(p.landing(), None);
}
#[test]
fn wire_payload_never_creates_directory_or_turn_and_ids_are_stable() {
    let c = candidate("/repo");
    let first = create_payload("p", &c);
    assert_eq!(first, create_payload("p", &c));
    assert_eq!(first["createWorkspaceRootIfMissing"], false);
    assert_eq!(first["commandId"], "onboarding:project:create:p");
    assert_eq!(import_payload("p", &c), json!({"projectId":"p","expectedWorkspaceRoot":"/repo"}));
}

#[test]
fn leaving_reentering_or_switching_discards_late_replies_even_after_access_returns() {
    let mut demand = Demand::default();
    let first = demand.begin(7);
    assert!(demand.accepts(first, Some(7), true));
    demand.cancel();
    assert!(!demand.accepts(first, Some(7), true));
    let second = demand.begin(7);
    assert!(!demand.accepts(first, Some(7), true));
    assert!(!demand.accepts(second, Some(8), true));
    assert!(!demand.accepts(second, None, true));
    assert!(!demand.accepts(second, Some(7), false));
    assert!(demand.accepts(second, Some(7), true));
}

#[test]
fn only_typed_project_rejections_discard_create_ids() {
    let failure = |tag: Option<&str>, error: Value| crate::rpc::RpcFailure {
        tag: tag.map(str::to_owned),
        message: "request failed".into(),
        error,
    };
    for error in [json!({"message":"projection store unavailable"}), json!({"message":"malformed response"}), json!({"_tag":"FutureError"})] {
        assert!(!confirmed_project_rejection(&failure(None, error)));
    }
    for cause in [
        json!({"name":"ProjectOperationError","message":"read-project failed"}),
        json!({"_tag":"ProjectOperationError","operation":"dispatch-project-command","cause":{"_tag":"EventSinkV2Error"}}),
        Value::Null,
    ] {
        assert!(!confirmed_project_rejection(&failure(Some("ProjectMutationError"), json!({"_tag":"ProjectMutationError","cause":cause}))));
    }
    assert!(!confirmed_project_rejection(&failure(Some("FutureError"), json!({"_tag":"FutureError"}))));
    for cause in [
        json!({"name":"ProjectConflictError","message":"Workspace already belongs to another project"}),
        json!({"name":"ProjectNotFoundError","message":"Project was not found"}),
    ] {
        assert!(confirmed_project_rejection(&failure(Some("ProjectMutationError"), json!({"_tag":"ProjectMutationError","cause":cause}))));
    }
}

#[test]
fn retry_counts_are_per_attempt_while_successes_and_landing_survive() {
    let key = key("local", "/repo");
    let mut p = Progress::default();
    p.begin_attempt(vec![key.clone()]);
    p.attempts.insert(key.clone(), "stable".into());
    p.record(key.clone(), "p".into(), ImportResult { imported_count: 2, skipped_count: 1 });
    p.failed_attempt(&key, false);
    p.begin_attempt(vec![key.clone()]);
    assert_eq!((p.imported, p.skipped, p.failures), (0, 0, 0));
    assert_eq!(p.landing(), Some("p"));
    assert_eq!(p.attempts[&key], "stable");
    p.record(key.clone(), "p".into(), ImportResult { imported_count: 3, skipped_count: 0 });
    assert_eq!(p.imported, 3);
    assert_eq!(p.completed[&key], "p");
    p.begin_attempt(vec![key.clone()]);
    assert_eq!(p.completed[&key], "p");
    assert_eq!(p.landing(), Some("p"));
}

#[test]
fn explicit_skip_after_revocation_finishes_locally_without_waiting_for_projects() {
    let key = key("local", "/repo");
    let mut p = Progress::default();
    p.begin_attempt(vec![key.clone()]);
    p.record(key, "p".into(), ImportResult { imported_count: 2, skipped_count: 1 });
    assert_eq!(p.completion(FinishIntent::Import, false, |_| true), Completion::Denied);
    assert_eq!(p.completion(FinishIntent::Skip, false, |id| id == "p"), Completion::Ready(Some("p".into())));
    assert_eq!(p.completion(FinishIntent::Skip, false, |_| false), Completion::Ready(None));
    assert_eq!(p.completion(FinishIntent::Import, true, |_| false), Completion::Wait("p".into()));
}
