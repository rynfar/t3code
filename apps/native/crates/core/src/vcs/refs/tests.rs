use super::*;
use crate::vcs::{list_refs, method};
use serde_json::json;

#[test]
fn invalidation_bypasses_mounted_and_other_repository_snapshots_but_shares_search_freshness() {
    let mut cache = RefCache::default();
    let input = list_refs("/repo", None, None, 100);
    let initial = cache.begin(input.clone());
    assert_eq!(initial.input, json!({"cwd":"/repo","limit":100}));
    assert!(cache.commit(&initial));
    assert!(cache.settle(method::VCS_REFRESH_STATUS));
    let mounted = cache.begin(input);
    assert_eq!(mounted.input, json!({"cwd":"/repo","limit":100,"refresh":true}));
    assert!(cache.commit(&mounted));
    let other = cache.begin(list_refs("/other-repo", None, None, 100));
    assert_eq!(other.input, json!({"cwd":"/other-repo","limit":100,"refresh":true}));
    assert!(cache.commit(&other));
    let search = cache.begin(list_refs("/other-repo", Some("release"), None, 100));
    assert_eq!(search.input, json!({"cwd":"/other-repo","query":"release","limit":100}));
    assert_eq!(
        cache.begin(list_refs("/other-repo", Some("release"), Some(100), 100)).input,
        json!({"cwd":"/other-repo","query":"release","cursor":100,"limit":100})
    );
    assert_eq!(RefCache::default().revision(), 0, "other environments and registries are independent");
}

#[test]
fn ref_affecting_commands_including_pr_checkout_invalidate_on_settle_even_after_failure() {
    let mut cache = RefCache::default();
    // Settlement has no success predicate: a failed pull can already have fetched refs.
    for (index, method) in [
        method::VCS_PULL,
        method::VCS_REFRESH_STATUS,
        "git.preparePullRequestThread",
        "vcs.createWorktree",
        "vcs.removeWorktree",
        method::VCS_CREATE_REF,
        method::VCS_SWITCH_REF,
        method::VCS_INIT,
    ]
    .iter()
    .enumerate()
    {
        assert!(cache.settle(method));
        assert_eq!(cache.revision(), index as u64 + 1);
    }
    assert!(!cache.settle("git.resolvePullRequest"));
    assert!(!cache.settle(method::VCS_LIST_REFS));
    assert_eq!(cache.revision(), 8);
}

#[test]
fn superseded_replies_cannot_commit_or_mark_the_server_snapshot_fresh() {
    let mut cache = RefCache::default();
    let old = cache.begin(list_refs("/repo", None, None, 100));
    cache.settle(method::VCS_PULL);
    assert!(!cache.commit(&old));
    let refreshing = cache.begin(list_refs("/repo", None, None, 100));
    cache.settle("git.preparePullRequestThread");
    assert!(!cache.commit(&refreshing));
    let current = cache.begin(list_refs("/repo", None, None, 100));
    assert_eq!(current.input["refresh"], true);
    assert!(cache.commit(&current));
    assert!(cache.begin(list_refs("/repo", None, None, 100)).input.get("refresh").is_none());
}

#[test]
fn concurrent_keys_bypass_until_a_successful_current_reply_commits() {
    let mut cache = RefCache::default();
    cache.settle(method::VCS_REFRESH_STATUS);
    let first = cache.begin(list_refs("/repo", None, None, 100));
    let search = cache.begin(json!({"cwd":"/repo","query":"release","cursor":100,"limit":100,"refKind":"remote","includeMatchingRemoteRefs":true}));
    assert_eq!(
        search.input,
        json!({"cwd":"/repo","query":"release","cursor":100,"limit":100,"refKind":"remote","includeMatchingRemoteRefs":true,"refresh":true})
    );
    // A failed first request never commits, so a retry must still bypass.
    assert_eq!(cache.begin(first.input.clone()).input["refresh"], true);
    assert!(cache.commit(&search));
    assert!(cache.commit(&first));
    assert!(cache.begin(list_refs("/repo", None, Some(100), 100)).input.get("refresh").is_none());
    assert_eq!(cache.begin(list_refs("/different", None, None, 100)).input["refresh"], true);
    assert_eq!(cache.begin(json!({"cwd":"/repo","limit":100,"refresh":true})).input["refresh"], true);
}

fn page(names: &[&str], next: Option<u64>) -> crate::vcs::ListRefsResult {
    serde_json::from_value(json!({"refs": names.iter().map(|name| json!({"name":name})).collect::<Vec<_>>(), "nextCursor":next,"totalCount":names.len()})).unwrap()
}

#[test]
fn refreshed_picker_replaces_old_refs_and_rejects_stale_search_page_and_checkout_replies() {
    let mut list = RefListing::default();
    let old = list.begin("", false).unwrap();
    let fresh = list.begin("", false).unwrap();
    assert!(!list.finish(&old, Ok(vec![page(&["deleted-outside-t3"], None)])));
    assert!(list.loading);
    assert!(list.finish(&fresh, Ok(vec![page(&["created-outside-t3"], Some(100))])));
    let mut filter = crate::vcs::RefFilter::default();
    filter.update(&list.refs, list.revision, "created");
    assert_eq!(filter.indices(), &[0]);
    let pagination = list.begin("", true).unwrap();
    assert_eq!(pagination.cursors, [Some(100)]);
    assert!(list.begin("", true).is_none(), "duplicate next pages are suppressed");
    let search = list.begin("release", false).unwrap();
    assert!(!list.finish(&pagination, Ok(vec![page(&["old-page"], None)])));
    assert!(!list.finish(&fresh, Err("late failure".into())));
    assert!(list.finish(&search, Ok(vec![page(&["release"], Some(100))])));
    let pagination = list.begin("release", true).unwrap();
    assert!(list.finish(&pagination, Ok(vec![page(&["release", "release-2"], None)])));
    assert_eq!(list.refs.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["release", "release-2"]);
    let checkout = list.begin("release", false).unwrap();
    list.reset();
    assert!(!list.finish(&checkout, Ok(vec![page(&["other-checkout"], None)])));
    assert!(list.refs.is_empty());
}

#[test]
fn background_refresh_retains_a_nonempty_paginated_listing_even_on_failure() {
    let mut list = RefListing::default();
    let first = list.begin("", false).unwrap();
    assert!(list.finish(&first, Ok(vec![page(&["main", "old"], Some(100))])));
    let next = list.begin("", true).unwrap();
    assert!(list.finish(&next, Ok(vec![page(&["later"], Some(200))])));
    let retained = list.refs.clone();
    let revision = list.revision;
    let refresh = list.begin("", false).unwrap();
    assert_eq!(refresh.cursors, [None, Some(100)]);
    assert!(!list.loading_next_page);
    assert_eq!(list.refs, retained, "pending refresh keeps all mounted pages");
    assert!(list.loaded);
    assert_eq!(list.next_cursor, Some(200));
    assert_eq!(list.revision, revision);
    assert!(list.finish(&refresh, Err("refresh failed".into())));
    assert_eq!(list.refs, retained, "failure retains rows and pagination");
    assert_eq!(list.next_cursor, Some(200));
    assert_eq!(list.error.as_deref(), Some("refresh failed"));
    let refresh = list.begin("", false).unwrap();
    assert_eq!(refresh.cursors, [None, Some(100)]);
    let selected = RefChoice::Ref("later".into());
    let indices = (0..list.refs.len()).collect::<Vec<_>>();
    assert_eq!(selected.index_in(&list.refs, &indices, None), Some(2));
    assert!(list.finish(&refresh, Ok(vec![page(&["new", "main"], Some(100)), page(&["later", "last"], Some(200))])));
    assert_eq!(list.refs.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["new", "main", "later", "last"]);
    assert_eq!(list.next_cursor, Some(200));
    assert!(list.error.is_none());
    let indices = (0..list.refs.len()).collect::<Vec<_>>();
    assert_eq!(selected.index_in(&list.refs, &indices, None), Some(2));
    let pending = list.begin("", true).unwrap();
    let refresh = list.begin("", false).unwrap();
    assert_eq!(refresh.cursors, [None, Some(100), Some(200)], "invalidation also restarts a newly mounted page");
    assert!(!list.finish(&pending, Ok(vec![page(&["stale-third-page"], None)])));
    assert!(list.finish(&refresh, Ok(vec![page(&["later", "main"], Some(100)), page(&["last"], Some(200)), page(&[], None)])));
    let indices = (0..list.refs.len()).collect::<Vec<_>>();
    assert_eq!(
        selected.index_in(&list.refs, &indices, None),
        Some(0),
        "the highlighted ref follows reordering across pages"
    );
    let refresh = list.begin("", false).unwrap();
    assert!(list.finish(&refresh, Ok(vec![page(&["main"], None), page(&[], None), page(&[], None)])));
    assert_eq!(selected.index_in(&list.refs, &[0], None), None, "removal must not select the ref now occupying that index");
    assert_eq!(list.next_cursor, None);
    assert_eq!(RefChoice::Create("main".into()).index_in(&list.refs, &[0], None), None);
    assert_eq!(RefChoice::Create("main".into()).index_in(&list.refs, &[0], Some("main")), Some(1));
    let search = list.begin("new-query", false).unwrap();
    assert_eq!(search.cursors, [None]);
    assert!(list.refs.is_empty(), "a different query starts a new listing");
}

#[test]
fn stacked_actions_and_publishing_invalidate_ref_snapshots() {
    let mut cache = RefCache::default();
    for method in [method::GIT_RUN_STACKED_ACTION, "sourceControl.publishRepository"] {
        let pending = cache.begin(list_refs("/repo", None, None, 100));
        assert!(cache.settle(method));
        assert!(!cache.commit(&pending));
        assert_eq!(cache.begin(list_refs("/repo", None, None, 100)).input["refresh"], true);
    }
}
