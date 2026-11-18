use serde_json::json;

use super::*;
use crate::auth::AuthSessionState;

fn local(ref_name: &str, insertions: u64) -> serde_json::Value {
    json!({
        "isRepo": true,
        "sourceControlProvider": { "kind": "github", "name": "GitHub", "baseUrl": "https://github.com" },
        "hasPrimaryRemote": true,
        "isDefaultRef": false,
        "refName": ref_name,
        "hasWorkingTreeChanges": insertions > 0,
        "workingTree": { "rootPath": "/repo", "files": [{ "path": "a.rs", "insertions": insertions, "deletions": 1 }], "insertions": insertions, "deletions": 1 },
        "futureField": 1
    })
}

fn event(value: serde_json::Value) -> VcsStatusEvent {
    serde_json::from_value(value).expect("decodes")
}

#[test]
fn status_stream_merges_local_and_remote_halves() {
    let snapshot = event(json!({
        "_tag": "snapshot",
        "local": local("feature", 3),
        "remote": { "hasUpstream": true, "aheadCount": 2, "behindCount": 0, "pr": { "number": 7, "title": "Add", "url": "https://x/7", "baseRef": "main", "headRef": "feature", "state": "open" } }
    }));
    let status = apply_status_event(None, snapshot).expect("status");
    assert_eq!(status.local.ref_name.as_deref(), Some("feature"));
    assert_eq!(status.remote.ahead_count, 2);
    assert!(status.has_open_pr());
    assert_eq!(status.provider_kind(), Some("github"));

    let status = apply_status_event(Some(status), event(json!({ "_tag": "localUpdated", "local": local("feature", 9) }))).expect("status");
    assert_eq!(status.local.working_tree.insertions, 9);
    assert_eq!(status.remote.ahead_count, 2, "a local update keeps the remote half");

    let status = apply_status_event(Some(status), event(json!({ "_tag": "remoteUpdated", "remote": null }))).expect("status");
    assert_eq!(status.local.working_tree.insertions, 9, "a remote update keeps the local half");
    assert_eq!(status.remote, VcsRemoteStatus::default());
    assert_eq!(status.remote.ahead_of_default_count, Some(0));
}

#[test]
fn remote_update_before_any_snapshot_assumes_an_empty_repo() {
    let status = apply_status_event(None, event(json!({ "_tag": "remoteUpdated", "remote": { "hasUpstream": true, "aheadCount": 0, "behindCount": 4, "pr": null } }))).expect("status");
    assert!(status.local.is_repo);
    assert_eq!(status.local.ref_name, None);
    assert_eq!(status.remote.behind_count, 4);
    assert_eq!(status.remote.ahead_of_default_count, None);
}

#[test]
fn unknown_status_events_change_nothing() {
    let current = apply_status_event(None, event(json!({ "_tag": "snapshot", "local": local("main", 0), "remote": null })));
    assert_eq!(apply_status_event(current.clone(), event(json!({ "_tag": "somethingNew", "x": 1 }))), current);
}

#[test]
fn refresh_result_decodes_as_one_flat_status() {
    let mut value = local("main", 2);
    value["hasUpstream"] = json!(false);
    value["aheadCount"] = json!(1);
    value["behindCount"] = json!(0);
    value["pr"] = json!(null);
    let status: VcsStatus = serde_json::from_value(value).expect("decodes");
    assert_eq!(status.local.ref_name.as_deref(), Some("main"));
    assert_eq!(status.remote.ahead_count, 1);
}

#[test]
fn changes_totals_prefer_branch_totals_unless_they_hide_a_dirty_tree() {
    let mut status = VcsStatus::default();
    status.local.working_tree.insertions = 5;
    status.local.working_tree.deletions = 2;
    assert_eq!(changes_totals(&status), (5, 2));
    status.local.branch_changes = Some(BranchChanges { base_ref: None, insertions: 0, deletions: 0 });
    assert_eq!(changes_totals(&status), (5, 2), "a zero branch total must not hide working tree changes");
    status.local.branch_changes = Some(BranchChanges { base_ref: Some("main".into()), insertions: 40, deletions: 3 });
    assert_eq!(changes_totals(&status), (40, 3));
    status.local.working_tree = WorkingTree::default();
    status.local.branch_changes = Some(BranchChanges { base_ref: None, insertions: 0, deletions: 0 });
    assert_eq!(changes_totals(&status), (0, 0));
}

#[test]
fn diff_counts_group_thousands() {
    assert_eq!(format_diff_count(0), "0");
    assert_eq!(format_diff_count(999), "999");
    assert_eq!(format_diff_count(1_000), "1,000");
    assert_eq!(format_diff_count(1_234_567), "1,234,567");
}

#[test]
fn pull_summaries_match_the_web_copy() {
    let pulled = PullResult { status: "pulled".into(), ref_name: "main".into(), upstream_ref: Some("origin/main".into()) };
    assert_eq!(pulled.summary(), ("Pulled".into(), "Updated main from origin/main".into()));
    let pulled = PullResult { upstream_ref: None, ..pulled };
    assert_eq!(pulled.summary().1, "Updated main from upstream");
    let skipped = PullResult { status: "skipped_up_to_date".into(), ref_name: "main".into(), upstream_ref: None };
    assert_eq!(skipped.summary(), ("Already up to date".into(), "main is already synchronized.".into()));
}

#[test]
fn ref_payloads_omit_blank_queries() {
    assert_eq!(list_refs("/r", Some("  "), None, 100), json!({ "cwd": "/r", "limit": 100 }));
    assert_eq!(list_refs("/r", Some("feat"), Some(100), 100), json!({ "cwd": "/r", "query": "feat", "cursor": 100, "limit": 100 }));
    assert_eq!(refresh_status("/r", true), json!({ "cwd": "/r", "localOnly": true }));
    assert_eq!(create_ref("/r", "x"), json!({ "cwd": "/r", "refName": "x", "switchRef": true }));
    let update = update_thread_branch("c", "t", Some("main"), None);
    assert_eq!(update["type"], "thread.metadata.update");
    assert_eq!(update["worktreePath"], serde_json::Value::Null);
}

#[test]
fn new_ref_names_replace_whitespace_runs() {
    assert_eq!(sanitize_new_ref_name("  my  new\tbranch "), "my-new-branch");
    assert_eq!(sanitize_new_ref_name("feature/x"), "feature/x");
}

#[test]
fn remote_refs_check_out_under_their_local_name() {
    assert_eq!(local_branch_name_from_remote_ref("origin/feature/x"), "feature/x");
    assert_eq!(local_branch_name_from_remote_ref("feature"), "feature");
    assert_eq!(local_branch_name_from_remote_ref("/x"), "/x");
    assert_eq!(local_branch_name_from_remote_ref("origin/"), "origin/");
}

fn vcs_ref(name: &str, default: bool, worktree: Option<&str>) -> VcsRef {
    VcsRef { name: name.into(), is_remote: None, remote_name: None, current: false, is_default: default, worktree_path: worktree.map(str::to_owned) }
}

#[test]
fn branch_selection_reuses_other_worktrees_and_leaves_one_for_the_default_ref() {
    let target = branch_selection_target("/repo", None, None, &vcs_ref("other", false, Some("/wt/other")));
    assert_eq!(target, BranchSelectionTarget { checkout_cwd: "/wt/other".into(), next_worktree_path: Some("/wt/other".into()), reuse_existing_worktree: true, stays_in_active_checkout: false });
    let target = branch_selection_target("/repo", Some("/repo"), Some("/wt/x"), &vcs_ref("main", false, Some("/repo")));
    assert_eq!(target.next_worktree_path, None);
    let target = branch_selection_target("/repo", None, Some("/wt/x"), &vcs_ref("main", true, None));
    assert_eq!(target, BranchSelectionTarget { checkout_cwd: "/repo".into(), next_worktree_path: None, reuse_existing_worktree: false, stays_in_active_checkout: false });
    let target = branch_selection_target("/repo", None, Some("/wt/x"), &vcs_ref("feature", false, None));
    assert_eq!(target, BranchSelectionTarget { checkout_cwd: "/wt/x".into(), next_worktree_path: Some("/wt/x".into()), reuse_existing_worktree: false, stays_in_active_checkout: true });
}

/// `#29908`: a subfolder project's own checkout is reported at the repository root.
#[test]
fn branch_selection_keeps_a_subfolder_project_in_its_own_checkout() {
    let target = branch_selection_target("/repo/nested-checkout/packages/app", Some("/repo/nested-checkout"), None, &vcs_ref("main", true, Some("/repo")));
    assert_eq!(target, BranchSelectionTarget { checkout_cwd: "/repo".into(), next_worktree_path: Some("/repo".into()), reuse_existing_worktree: true, stays_in_active_checkout: false });
    let target = branch_selection_target("/repo", None, Some("/repo/.t3/worktrees/feature-a"), &vcs_ref("b", false, Some("/repo/.t3/worktrees/feature-b")));
    assert_eq!(target.next_worktree_path.as_deref(), Some("/repo/.t3/worktrees/feature-b"));
    // The checkout root is `None` until the project's status loads.
    for (project_cwd, worktree, checkout_root) in [
        ("/repo", Some("/repo/.t3/worktrees/feature-a"), Some("/repo")),
        ("/repo/packages/app", Some("/repo/.t3/worktrees/feature-a"), Some("/repo")),
        ("/repo/packages/app", None, Some("/repo")),
        ("/repo/packages/app", Some("/repo/.t3/worktrees/feature-a"), None),
        ("/repo/packages/app", None, None),
    ] {
        let target = branch_selection_target(project_cwd, checkout_root, worktree, &vcs_ref("main", true, Some("/repo")));
        // A ref listed in the thread's own checkout may be stale, so it is checked out, not assumed.
        let expected = BranchSelectionTarget { checkout_cwd: "/repo".into(), next_worktree_path: None, reuse_existing_worktree: true, stays_in_active_checkout: worktree.is_none() };
        assert_eq!(target, expected, "{project_cwd} {worktree:?} {checkout_root:?}");
    }
}

#[test]
fn a_listed_ref_in_the_threads_own_checkout_switches_only_with_write_access() {
    let own = branch_selection_target("/repo/packages/app", Some("/repo"), None, &vcs_ref("main", true, Some("/repo")));
    assert!(!records_existing_worktree(&own, true));
    assert!(records_existing_worktree(&own, false));
    let other = branch_selection_target("/repo", None, None, &vcs_ref("b", false, Some("/wt/b")));
    assert!(records_existing_worktree(&other, true));
    let in_place = branch_selection_target("/repo", None, None, &vcs_ref("b", false, None));
    assert!(!records_existing_worktree(&in_place, false));
}

#[test]
fn branch_status_falls_back_to_local_status_events() {
    let branch: VcsBranchStatus = serde_json::from_value(json!({ "refName": "main", "isRepo": false })).unwrap();
    assert_eq!(branch, VcsBranchStatus { ref_name: Some("main".into()), is_repo: Some(false) });
    let older: VcsBranchStatus = serde_json::from_value(json!({ "refName": null })).unwrap();
    assert_eq!(older.is_repo, None);
    let snapshot = event(json!({ "_tag": "snapshot", "local": local("feature", 1), "remote": null }));
    assert_eq!(branch_status_from_status_event(snapshot), Some(VcsBranchStatus { ref_name: Some("feature".into()), is_repo: Some(true) }));
    let updated = event(json!({ "_tag": "localUpdated", "local": local("next", 0) }));
    assert_eq!(branch_status_from_status_event(updated).and_then(|status| status.ref_name).as_deref(), Some("next"));
    assert_eq!(branch_status_from_status_event(event(json!({ "_tag": "remoteUpdated", "remote": null }))), None);
    assert_eq!(branch_fallback_subscription("/repo"), json!({ "cwd": "/repo", "includeRemote": false }));
}

#[test]
fn branch_trigger_follows_the_checkout_unless_choosing_a_worktree_base() {
    assert_eq!(branch_toolbar_value(false, Some("thread"), Some("git")), Some("git".into()));
    assert_eq!(branch_toolbar_value(true, Some("thread"), Some("git")), Some("thread".into()));
    assert_eq!(branch_trigger_label(false, None, None, false), "Select ref");
    assert_eq!(branch_trigger_label(false, Some("main"), None, true), "main");
    assert_eq!(branch_trigger_label(true, Some("main"), Some(false), true), "From main (remote)");
    assert_eq!(branch_trigger_label(true, Some("main"), None, true), "From main");
    assert_eq!(pr_branch(Some("a"), Some("a")), Some("a"));
    assert_eq!(pr_branch(Some("a"), Some("b")), None);
}

#[test]
fn ref_search_also_matches_the_sanitized_query() {
    assert!(ref_matches_query("feature/new-thing", ""));
    assert!(ref_matches_query("Feature/New-Thing", "new-thing"));
    assert!(ref_matches_query("feature/new-thing", "new thing"));
    assert!(!ref_matches_query("main", "dev"));
}

#[test]
fn ref_filter_narrows_as_the_query_grows_and_rescans_new_listings() {
    let refs: Vec<VcsRef> = ["main", "feature/new-thing", "feature/old", "fix/new"].into_iter().map(|name| vcs_ref(name, false, None)).collect();
    let names = |filter: &RefFilter, refs: &[VcsRef]| filter.indices().iter().map(|&index| refs[index].name.clone()).collect::<Vec<_>>();
    let mut filter = RefFilter::default();
    filter.update(&refs, 1, "");
    assert_eq!(filter.indices().len(), 4);
    filter.update(&refs, 1, "new");
    assert_eq!(names(&filter, &refs), ["feature/new-thing", "fix/new"]);
    filter.update(&refs, 1, "new thing");
    assert_eq!(names(&filter, &refs), ["feature/new-thing"], "the sanitized query matches");
    filter.update(&refs, 1, "fe");
    assert_eq!(names(&filter, &refs), ["feature/new-thing", "feature/old"], "a shorter query scans every ref again");
    let more: Vec<VcsRef> = refs.iter().cloned().chain([vcs_ref("feature/z", false, None)]).collect();
    filter.update(&more, 2, "fe");
    assert_eq!(names(&filter, &more), ["feature/new-thing", "feature/old", "feature/z"], "a new listing rescans");
}

#[test]
fn changing_a_threads_branch_needs_source_control_write_and_operate() {
    let session = |value: serde_json::Value| -> AuthSessionState { serde_json::from_value(value).expect("decodes") };
    assert!(!can_change_thread_branch(None), "unknown grants allow nothing");
    assert!(can_change_thread_branch(Some(&session(json!({ "authenticated": true, "scopes": ["orchestration:operate"] })))), "legacy credentials imply source control from operate");
    assert!(!can_change_thread_branch(Some(&session(json!({ "authenticated": true, "permissions": ["source-control:write"] })))));
    assert!(!can_change_thread_branch(Some(&session(json!({ "authenticated": true, "permissions": ["orchestration:operate"] })))));
    assert!(can_change_thread_branch(Some(&session(json!({ "authenticated": true, "permissions": ["source-control:write", "orchestration:operate"] })))));
    assert!(can_operate_thread(Some(&session(json!({ "authenticated": true, "permissions": ["orchestration:operate"] })))));
}

#[test]
fn ref_badges_rank_current_worktree_remote_default() {
    let mut item = vcs_ref("main", true, Some("/repo"));
    assert_eq!(ref_badge(&item, Some("/repo")), Some("default"));
    item.current = true;
    assert_eq!(ref_badge(&item, Some("/repo")), Some("current"));
    let other = vcs_ref("x", false, Some("/wt/x"));
    assert_eq!(ref_badge(&other, Some("/repo")), Some("worktree"));
    let remote = VcsRef { is_remote: Some(true), ..vcs_ref("origin/x", false, None) };
    assert_eq!(ref_badge(&remote, Some("/repo")), Some("remote"));
    assert_eq!(ref_badge(&vcs_ref("x", false, None), Some("/repo")), None);
}

#[test]
fn preferred_editor_keeps_the_last_one_while_available() {
    let available = vec!["zed".to_owned(), "vscode".to_owned(), "unknown-editor".to_owned()];
    assert_eq!(editor_options(&available), ["vscode", "zed"]);
    assert_eq!(preferred_editor(None, &available), Some("vscode"));
    assert_eq!(preferred_editor(Some("zed"), &available), Some("zed"));
    assert_eq!(preferred_editor(Some("cursor"), &available), Some("vscode"));
    assert_eq!(preferred_editor(None, &[]), None);
    assert_eq!(editor_label("file-manager", true), "Finder");
    assert_eq!(editor_label("idea", true), "IntelliJ IDEA");
    assert_eq!(editor_label("nope", true), "Editor");
}
