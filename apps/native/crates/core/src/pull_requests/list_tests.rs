use std::collections::HashMap;

use super::*;
use crate::pull_requests::query::{DraftFilter, parse_query};

fn entry(number: u64, title: &str) -> ListEntry {
    ListEntry {
        stack: None,
        provider: "github".into(),
        host: "github.com".into(),
        project_id: "p".into(),
        project_title: "Project".into(),
        repository: "acme/app".into(),
        number,
        title: title.into(),
        url: format!("https://github.com/acme/app/pull/{number}"),
        author: Some(Actor { is_bot: None, login: "octo".into(), name: None, avatar_url: None }),
        head_branch: format!("branch-{number}"),
        base_branch: "main".into(),
        state: PrState::Open,
        is_draft: false,
        mergeability: Mergeability::Mergeable,
        additions: 0,
        deletions: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-02T00:00:00Z".into(),
        viewer_review_requested: false,
        labels: Vec::new(),
        review_decision: None,
        checks_state: None,
    }
}

fn viewers() -> HashMap<String, String> {
    HashMap::from([("github.com".to_owned(), "Me".to_owned())])
}

fn numbers(entries: &[ListEntry], rows: &[usize]) -> Vec<u64> {
    rows.iter().map(|index| entries[*index].number).collect()
}

#[test]
fn groups_authored_then_review_requested_then_others() {
    let mut mine = entry(1, "mine");
    mine.author = Some(Actor { is_bot: None, login: "me".into(), name: None, avatar_url: None });
    let mut asked = entry(2, "asked");
    asked.viewer_review_requested = true;
    let entries = vec![entry(3, "other"), asked, mine];
    let groups = group_by_involvement(&entries, &[0, 1, 2], &viewers());
    let keys: Vec<GroupKey> = groups.iter().map(|group| group.key).collect();
    assert_eq!(keys, vec![GroupKey::Authored, GroupKey::ReviewRequested, GroupKey::Others]);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![1]);
    assert_eq!(groups[1].label, "Review requested");
}

#[test]
fn filters_match_draft_review_labels_and_author_me() {
    let mut row = entry(7, "x");
    row.is_draft = true;
    row.labels = vec![Label { name: "Bug".into(), color: None }];
    row.author = Some(Actor { is_bot: None, login: "Me".into(), name: None, avatar_url: None });
    let viewer = Some("me");
    assert!(matches_filters(&row, &parse_query("draft:true label:bug author:@me").filters, viewer));
    assert!(!matches_filters(&row, &parse_query("draft:false").filters, viewer));
    assert!(!matches_filters(&row, &parse_query("-label:bug").filters, viewer));
    assert!(matches_filters(&row, &parse_query("review:none").filters, viewer));
    row.review_decision = Some(ReviewDecision::Approved);
    assert!(!matches_filters(&row, &parse_query("review:none").filters, viewer));
    assert!(matches_filters(&row, &ListFilters { draft: Some(DraftFilter::Only), ..Default::default() }, viewer));
}

#[test]
fn readiness_ranks_approved_passing_first_and_conflicts_last() {
    let mut ready = entry(1, "ready");
    ready.checks_state = Some(ChecksState::Passing);
    ready.review_decision = Some(ReviewDecision::Approved);
    let mut passing = entry(2, "passing");
    passing.checks_state = Some(ChecksState::Passing);
    let mut conflicted = entry(3, "conflicted");
    conflicted.mergeability = Mergeability::Conflicting;
    let mut closed = entry(4, "closed");
    closed.state = PrState::Closed;
    let entries = vec![conflicted, closed, passing, ready];
    let mut groups = vec![Group { key: GroupKey::Others, label: "", entries: vec![0, 1, 2, 3] }];
    sort_groups(&entries, &mut groups, Sort::Ready, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![1, 2, 4, 3]);
}

#[test]
fn size_sorts_put_measured_rows_first() {
    let mut big = entry(1, "big");
    big.additions = 500;
    let mut small = entry(2, "small");
    small.deletions = 3;
    let entries = vec![entry(3, "unmeasured"), small, big];
    let mut groups = vec![Group { key: GroupKey::Others, label: "", entries: vec![0, 1, 2] }];
    sort_groups(&entries, &mut groups, Sort::Largest, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![1, 2, 3]);
    sort_groups(&entries, &mut groups, Sort::Smallest, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![2, 1, 3]);
}

#[test]
fn blocked_on_me_ranks_my_conflicts_before_my_drafts() {
    let me = Some(Actor { is_bot: None, login: "me".into(), name: None, avatar_url: None });
    let mut draft = entry(1, "draft");
    draft.is_draft = true;
    draft.author = me.clone();
    let mut conflicted = entry(2, "conflicted");
    conflicted.mergeability = Mergeability::Conflicting;
    conflicted.author = me;
    let entries = vec![draft, conflicted];
    let mut groups = group_by_involvement(&entries, &[0, 1], &viewers());
    sort_groups(&entries, &mut groups, Sort::Blocked, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![2, 1]);
}

#[test]
fn recency_and_creation_sorts() {
    let mut old = entry(1, "old");
    old.created_at = "2025-01-01T00:00:00Z".into();
    old.updated_at = "2026-03-01T00:00:00Z".into();
    let new = entry(2, "new");
    let entries = vec![old, new];
    let mut groups = vec![Group { key: GroupKey::Others, label: "", entries: vec![0, 1] }];
    sort_groups(&entries, &mut groups, Sort::Newest, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![2, 1]);
    sort_groups(&entries, &mut groups, Sort::Oldest, "", Involvement::All);
    assert_eq!(numbers(&entries, &groups[0].entries), vec![1, 2]);
    assert_eq!(compare_by_recency(&entries[0], &entries[1]), Ordering::Less);
}

#[test]
fn search_scores_numbers_titles_and_elsewhere() {
    let row = entry(42, "Fix model picker");
    assert_eq!(score_match(&row, "#42"), 100);
    assert_eq!(score_match(&row, "43"), 0);
    assert_eq!(score_match(&row, "fix model picker"), 90);
    assert_eq!(score_match(&row, "picker model"), 70);
    assert_eq!(score_match(&row, "branch-42"), 60);
    assert!(matched_elsewhere(&row, "description words"));
    assert!(matches_query(&row, "acme/app"));
    assert!(!matches_query(&row, "nothing"));
}

#[test]
fn shown_groups_keep_host_matches_only_when_answered() {
    let entries = vec![entry(1, "alpha"), entry(2, "beta")];
    let providers = vec![ProviderSummary {
        host: "github.com".into(),
        kind: "github".into(),
        searches_on_host: true,
        actions: None,
        project_count: 1,
        configured: true,
        detail: None,
    }];
    let viewers = viewers();
    let filters = ListFilters::default();
    let input = |answered| Shown {
        entries: &entries,
        viewers: &viewers,
        providers: &providers,
        involvement: Involvement::Reviewing,
        filters: &filters,
        text: "alpha",
        answered,
        sort: Sort::Updated,
    };
    assert!(shown_groups(&input(true))[0].entries.is_empty());
    let mut asked = entries.clone();
    asked.iter_mut().for_each(|row| row.viewer_review_requested = true);
    let answered = Shown { entries: &asked, ..input(true) };
    assert_eq!(numbers(&asked, &shown_groups(&answered)[0].entries), vec![1, 2]);
    let local = Shown { entries: &asked, ..input(false) };
    assert_eq!(numbers(&asked, &shown_groups(&local)[0].entries), vec![1]);
}

#[test]
fn quick_actions_follow_state_and_host() {
    let row = entry(1, "x");
    let host = host_actions(&[], &row);
    assert_eq!(quick_actions(PrState::Open, false, &host), vec![QuickAction::Close, QuickAction::Merge]);
    assert_eq!(quick_actions(PrState::Open, true, &host), vec![QuickAction::Close, QuickAction::Ready]);
    assert_eq!(quick_actions(PrState::Closed, false, &host), vec![QuickAction::Reopen]);
    assert!(quick_actions(PrState::Merged, false, &host).is_empty());
    let reported = vec![ProviderSummary {
        host: "GitHub.com".into(),
        kind: "github".into(),
        searches_on_host: false,
        actions: Some(vec![Action::Close]),
        project_count: 1,
        configured: true,
        detail: None,
    }];
    assert_eq!(quick_actions(PrState::Open, false, &host_actions(&reported, &row)), vec![QuickAction::Close]);
}

#[test]
fn facets_count_authors_and_labels_in_state() {
    let mut a = entry(1, "a");
    a.labels = vec![Label { name: "bug".into(), color: Some("ff0000".into()) }];
    let mut b = entry(2, "b");
    b.state = PrState::Merged;
    b.labels = vec![Label { name: "Bug".into(), color: None }];
    let (authors, labels) = facets(&[a.clone(), b], ListState::Open);
    assert_eq!(authors.len(), 1);
    assert_eq!((authors[0].count, authors[0].merged), (1, 1));
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].count, 1);
    assert_eq!(label_color(Some("#FF0000")), Some(0xff0000));
    assert_eq!(label_color(Some("red")), None);
    assert!(needs_stats(&a));
}
