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
    let (old, _) = list.begin("", false).unwrap();
    let (fresh, _) = list.begin("", false).unwrap();
    assert!(!list.finish(old, Ok(page(&["deleted-outside-t3"], None))));
    assert!(list.loading);
    assert!(list.finish(fresh, Ok(page(&["created-outside-t3"], Some(100)))));
    let mut filter = crate::vcs::RefFilter::default();
    filter.update(&list.refs, list.revision, "created");
    assert_eq!(filter.indices(), &[0]);
    let (pagination, cursor) = list.begin("", true).unwrap();
    assert_eq!(cursor, Some(100));
    assert!(list.begin("", true).is_none(), "duplicate next pages are suppressed");
    let (search, _) = list.begin("release", false).unwrap();
    assert!(!list.finish(pagination, Ok(page(&["old-page"], None))));
    assert!(!list.finish(fresh, Err("late failure".into())));
    assert!(list.finish(search, Ok(page(&["release"], Some(100)))));
    let (pagination, _) = list.begin("release", true).unwrap();
    assert!(list.finish(pagination, Ok(page(&["release", "release-2"], None))));
    assert_eq!(list.refs.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["release", "release-2"]);
    let (checkout, _) = list.begin("release", false).unwrap();
    list.reset();
    assert!(!list.finish(checkout, Ok(page(&["other-checkout"], None))));
    assert!(list.refs.is_empty());
}
