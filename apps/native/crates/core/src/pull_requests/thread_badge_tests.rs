use serde_json::{Value, json};

use super::*;

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

fn snapshot(extra: Value) -> Value {
    merge(
        json!({
            "state": "open", "title": "Change", "headBranch": "feature", "baseBranch": "main", "isDraft": false,
            "updatedAt": "2026-01-01T00:00:00.000Z", "syncedAt": "2026-01-01T00:00:00.000Z"
        }),
        extra,
    )
}

fn link(number: u64, extra: Value) -> ThreadLink {
    let base = json!({
        "host": "github.com", "repository": "pingdotgg/t3code", "number": number,
        "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"), "source": "manual",
        "linkedAt": format!("2026-01-01T00:00:{number:02}.000Z"), "snapshot": null, "stack": null
    });
    serde_json::from_value(merge(base, extra)).unwrap()
}

fn stack(id: &str, layers: &[(u64, &str, &str)]) -> Value {
    json!({
        "kind": "native", "id": id, "number": 1, "url": "https://github.com/pingdotgg/t3code/stacks/1", "base": "main",
        "layers": layers.iter().map(|(number, head, state)| json!({ "number": number, "headBranch": head, "state": state })).collect::<Vec<_>>()
    })
}

fn numbers(links: &[&ThreadLink]) -> Vec<u64> {
    links.iter().map(|link| link.number).collect()
}

fn chain_numbers(links: &[ThreadLink]) -> Vec<Vec<u64>> {
    resolve_thread_pull_request_chains(links).iter().map(|chain| numbers(&chain.layers)).collect()
}

fn current_open(links: &[ThreadLink]) -> Option<Vec<u64>> {
    match resolve_thread_current_pull_request(links)? {
        ThreadCurrentPullRequest::Stack { open, .. } => Some(numbers(&open)),
        ThreadCurrentPullRequest::Single(_) => None,
    }
}

fn single(links: &[ThreadLink]) -> Option<u64> {
    match resolve_thread_current_pull_request(links)? {
        ThreadCurrentPullRequest::Single(link) => Some(link.number),
        ThreadCurrentPullRequest::Stack { .. } => None,
    }
}

#[test]
fn current_selects_the_same_terminal_link_as_stable_sorting_across_input_orders() {
    let dates = ["invalid", "1969-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z"];
    for offset in 0..dates.len() {
        let links: Vec<ThreadLink> =
            (0..12).map(|index| link(index as u64 + 1, json!({ "snapshot": snapshot(json!({ "state": "merged", "updatedAt": dates[(index + offset) % dates.len()] })) }))).collect();
        let mut reversed = links.clone();
        reversed.reverse();
        for input in [links, reversed] {
            let mut expected: Vec<&ThreadLink> = input.iter().collect();
            expected.sort_by_key(|link| std::cmp::Reverse(crate::timeline::parse_iso_ms(link.snapshot.as_ref().and_then(|s| s.updated_at.as_deref()).unwrap_or(&link.linked_at)).unwrap_or(0)));
            assert_eq!(single(&input), Some(expected[0].number));
        }
    }
}

#[test]
fn current_preserves_input_order_when_open_chains_have_equal_link_dates() {
    let at = json!({ "linkedAt": "2026-01-01T00:00:00.000Z" });
    assert_eq!(current_open(&[link(3, at.clone()), link(1, at.clone()), link(2, at)]), Some(vec![3, 1, 2]));
}

#[test]
fn current_keeps_the_first_terminal_link_on_ties_and_treats_invalid_update_dates_as_zero() {
    let first = link(3, json!({ "snapshot": snapshot(json!({ "state": "merged", "updatedAt": "invalid" })) }));
    let second = link(1, json!({ "snapshot": snapshot(json!({ "state": "closed", "updatedAt": "1970-01-01T00:00:00.000Z" })) }));
    assert_eq!(single(&[first.clone(), second.clone()]), Some(3));
    assert_eq!(single(&[second, first]), Some(1));
}

#[test]
fn current_prefers_the_single_open_link_over_terminal_ones() {
    let links = [
        link(1, json!({ "snapshot": snapshot(json!({ "state": "merged" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "state": "open" })) })),
        link(3, json!({ "snapshot": snapshot(json!({ "state": "closed" })) })),
    ];
    assert_eq!(single(&links), Some(2));
    assert_eq!(resolve_thread_current_pull_request_link(&links).map(|link| link.number), Some(2));
}

#[test]
fn current_reports_a_stack_when_several_links_are_open_and_puts_the_highest_layer_on_top() {
    let native = stack("s1", &[(10, "a", "open"), (11, "b", "open")]);
    let links = [link(11, json!({ "snapshot": snapshot(json!({})), "stack": native })), link(10, json!({ "snapshot": snapshot(json!({})), "stack": native }))];
    assert_eq!(current_open(&links), Some(vec![11, 10]));
    assert_eq!(resolve_thread_current_pull_request_link(&links).map(|link| link.number), Some(11));
}

#[test]
fn current_orders_an_open_set_without_stack_data_by_most_recent_link() {
    let links = [link(1, json!({})), link(2, json!({}))];
    assert_eq!(resolve_thread_current_pull_request_link(&links).map(|link| link.number), Some(2));
    assert!(current_open(&links).is_some());
    assert_eq!(resolve_thread_current_pull_request(&[]), None);
}

#[test]
fn current_falls_back_to_the_most_recently_updated_terminal_link() {
    let links = [
        link(1, json!({ "snapshot": snapshot(json!({ "state": "merged", "updatedAt": "2026-01-03T00:00:00.000Z" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "state": "closed", "updatedAt": "2026-01-02T00:00:00.000Z" })) })),
    ];
    assert_eq!(single(&links), Some(1));
}

#[test]
fn chains_link_base_to_head_within_a_repository_bottom_to_top() {
    let links = [
        link(3, json!({ "snapshot": snapshot(json!({ "headBranch": "c", "baseBranch": "b" })) })),
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "a", "baseBranch": "main" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "b", "baseBranch": "a" })) })),
        link(9, json!({ "snapshot": snapshot(json!({ "headBranch": "solo", "baseBranch": "main" })) })),
    ];
    let chains = resolve_thread_pull_request_chains(&links);
    assert_eq!(chains.iter().map(|chain| (chain.kind, numbers(&chain.layers))).collect::<Vec<_>>(), vec![(ChainKind::Derived, vec![1, 2, 3]), (ChainKind::Derived, vec![9])]);
}

#[test]
fn chains_keep_every_link_built_on_the_same_parent_under_it() {
    let links = [
        link(3, json!({ "snapshot": snapshot(json!({ "headBranch": "c", "baseBranch": "a" })) })),
        link(5, json!({ "snapshot": snapshot(json!({ "headBranch": "p", "baseBranch": "main" })) })),
        link(6, json!({ "snapshot": snapshot(json!({ "headBranch": "q", "baseBranch": "p" })) })),
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "a", "baseBranch": "p" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "b", "baseBranch": "a" })) })),
        link(9, json!({ "snapshot": snapshot(json!({ "headBranch": "solo", "baseBranch": "main" })) })),
    ];
    let chains = resolve_thread_pull_request_chains(&links);
    assert_eq!(
        chains.iter().map(|chain| (numbers(&chain.layers), chain.depths.clone())).collect::<Vec<_>>(),
        vec![(vec![5, 6, 1, 3, 2], vec![0, 1, 1, 2, 2]), (vec![9], vec![0])]
    );
    assert_eq!(resolve_thread_pull_request_badge(&links[..5]), Some(ThreadPullRequestBadge::Stack { layers: 5, state: BadgeState::Open }));
}

#[test]
fn current_points_at_the_highest_layer_of_a_branched_stack_not_the_last_sibling_listed() {
    let branched = |state: &str| {
        [
            link(1, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "a" })) })),
            link(2, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "b", "baseBranch": "a" })) })),
            link(3, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "c", "baseBranch": "b" })) })),
            link(4, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "d", "baseBranch": "a" })) })),
        ]
    };
    assert_eq!(current_open(&branched("open")), Some(vec![3, 4, 2, 1]));
    assert_eq!(single(&branched("merged")), Some(3));
}

#[test]
fn chains_use_a_later_complete_native_snapshot_for_layer_order_and_the_current_pull_request() {
    let links: Vec<ThreadLink> = [18u64, 20, 83, 84]
        .iter()
        .map(|&number| {
            let order: &[u64] = if number < 83 { &[79, 37, 38, 39, 76, 18, 20] } else { &[79, 37, 38, 39, 76, 18, 20, 83, 84] };
            let heads: Vec<String> = order.iter().map(|layer| format!("pr-{layer}")).collect();
            let layers: Vec<(u64, &str, &str)> = order.iter().zip(&heads).map(|(layer, head)| (*layer, head.as_str(), "open")).collect();
            let mut native = stack("80", &layers);
            native["number"] = json!(80);
            link(number, json!({ "linkedAt": "2026-01-01T00:00:00.000Z", "snapshot": snapshot(json!({})), "stack": native }))
        })
        .collect();
    assert_eq!(chain_numbers(&links), vec![vec![18, 20, 83, 84]]);
    assert_eq!(current_open(&links), Some(vec![84, 83, 20, 18]));
    assert_eq!(resolve_thread_current_pull_request_link(&links).map(|link| link.number), Some(84));
}

#[test]
fn chains_use_the_earliest_longest_partial_snapshot_and_append_missing_members_in_stored_order() {
    let links: Vec<ThreadLink> = [vec![3u64, 2], vec![9, 2, 3], vec![9, 3, 2], vec![9, 3, 2]]
        .iter()
        .enumerate()
        .map(|(index, order)| {
            let heads: Vec<String> = order.iter().map(|layer| format!("pr-{layer}")).collect();
            let layers: Vec<(u64, &str, &str)> = order.iter().zip(&heads).map(|(layer, head)| (*layer, head.as_str(), "open")).collect();
            link(index as u64 + 1, json!({ "stack": stack("s1", &layers) }))
        })
        .collect();
    assert_eq!(chain_numbers(&links), vec![vec![2, 3, 1, 4]]);
}

#[test]
fn chains_use_the_native_stack_order_when_the_host_provides_one() {
    let native = stack("s1", &[(5, "a", "merged"), (6, "b", "open")]);
    let links = [
        link(6, json!({ "snapshot": snapshot(json!({ "headBranch": "b", "baseBranch": "main" })), "stack": native })),
        link(5, json!({ "snapshot": snapshot(json!({ "state": "merged", "headBranch": "a", "baseBranch": "main" })), "stack": native })),
        link(8, json!({})),
    ];
    let chains = resolve_thread_pull_request_chains(&links);
    assert_eq!(chains.iter().map(|chain| (chain.kind, numbers(&chain.layers))).collect::<Vec<_>>(), vec![(ChainKind::Native, vec![5, 6]), (ChainKind::Derived, vec![8])]);
}

#[test]
fn targets_the_top_of_a_derived_chain_despite_a_later_bottom_update_and_link() {
    for (state, badge_state) in [("open", BadgeState::Open), ("merged", BadgeState::Merged), ("closed", BadgeState::Closed)] {
        let bottom = link(2, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "base", "updatedAt": "2026-02-01T00:00:00.000Z" })) }));
        let top = link(1, json!({ "snapshot": snapshot(json!({ "state": state, "headBranch": "top", "baseBranch": "base" })) }));
        let links = [bottom, top];
        if state == "open" {
            assert_eq!(current_open(&links), Some(vec![1, 2]));
        } else {
            assert_eq!(single(&links), Some(1));
        }
        assert_eq!(resolve_thread_pull_request_badge(&links), Some(ThreadPullRequestBadge::Stack { layers: 2, state: badge_state }));
    }
}

#[test]
fn keeps_the_top_native_layer_after_completion_and_treats_an_unsynced_layer_as_open() {
    let native = stack("native-1", &[(2, "base", "merged"), (1, "top", "merged")]);
    let bottom = link(2, json!({ "stack": native, "snapshot": snapshot(json!({ "state": "merged", "updatedAt": "2026-02-01T00:00:00.000Z" })) }));
    let top = link(1, json!({ "stack": native, "snapshot": snapshot(json!({ "state": "merged" })) }));
    assert_eq!(single(&[bottom.clone(), top.clone()]), Some(1));
    let unsynced = ThreadLink { snapshot: None, ..top };
    assert_eq!(resolve_thread_pull_request_badge(&[bottom, unsynced]), Some(ThreadPullRequestBadge::Stack { layers: 2, state: BadgeState::Open }));
}

#[test]
fn badge_uses_aggregate_state_and_distinguishes_unrelated_work_from_a_stack() {
    let bottom = link(1, json!({ "snapshot": snapshot(json!({ "state": "merged", "headBranch": "base" })) }));
    let top = link(2, json!({ "snapshot": snapshot(json!({ "state": "closed", "baseBranch": "base" })) }));
    assert_eq!(resolve_thread_pull_request_badge(&[bottom.clone(), top.clone()]), Some(ThreadPullRequestBadge::Stack { layers: 2, state: BadgeState::Closed }));
    assert_eq!(resolve_thread_pull_request_badge(&[bottom, top, link(3, json!({}))]), Some(ThreadPullRequestBadge::PullRequest { others: 2, state: BadgeState::Open }));
    assert_eq!(resolve_thread_pull_request_badge(&[link(3, json!({ "source": "stack-dismissed" }))]), None);
    let draft = link(4, json!({ "snapshot": snapshot(json!({ "isDraft": true })) }));
    assert_eq!(resolve_thread_pull_request_badge(std::slice::from_ref(&draft)), Some(ThreadPullRequestBadge::PullRequest { others: 0, state: BadgeState::Draft }));
    assert_eq!(resolve_thread_pull_request_badge(&[draft, link(5, json!({}))]).map(|badge| badge.state()), Some(BadgeState::Open));
}

#[test]
fn keeps_branch_matching_case_sensitive_while_ignoring_repository_case() {
    let bottom = link(1, json!({ "repository": "PingDotGG/T3code", "snapshot": snapshot(json!({ "headBranch": "Base" })) }));
    let top = link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "top", "baseBranch": "base" })) }));
    assert_eq!(resolve_thread_pull_request_chains(&[bottom.clone(), top]).len(), 2);
    let matched = link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "top", "baseBranch": "Base" })) }));
    assert_eq!(chain_numbers(&[bottom, matched]), vec![vec![1, 2]]);
}

#[test]
fn preserves_cyclic_links_without_presenting_a_guessed_stack_order() {
    let links = [
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "a", "baseBranch": "b" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "b", "baseBranch": "a" })) })),
    ];
    assert_eq!(chain_numbers(&links), vec![vec![1], vec![2]]);
    assert_eq!(resolve_thread_current_pull_request_link(&links).map(|link| link.number), Some(2));
    assert!(current_open(&links).is_some());
    assert_eq!(resolve_thread_pull_request_badge(&links), Some(ThreadPullRequestBadge::PullRequest { others: 1, state: BadgeState::Open }));
}

#[test]
fn lists_a_pull_request_once_when_two_links_share_its_identity() {
    let links = [
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "a" })) })),
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "b", "baseBranch": "a" })) })),
        link(1, json!({ "snapshot": snapshot(json!({ "headBranch": "c" })) })),
    ];
    let chains = resolve_thread_pull_request_chains(&links);
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].layers.len(), 1);
    assert!(std::ptr::eq(chains[0].layers[0], &links[0]));
}

#[test]
fn does_not_guess_a_parent_when_a_head_branch_was_reused() {
    let links = [
        link(1, json!({ "snapshot": snapshot(json!({ "state": "merged", "headBranch": "reused" })) })),
        link(2, json!({ "snapshot": snapshot(json!({ "headBranch": "reused" })) })),
        link(3, json!({ "snapshot": snapshot(json!({ "headBranch": "top", "baseBranch": "reused" })) })),
    ];
    assert_eq!(chain_numbers(&links), vec![vec![1], vec![2], vec![3]]);
}

fn thread_pr(number: u64, state: PrState, is_draft: bool, url: &str) -> ThreadPr {
    ThreadPr { number, title: "Fix login".into(), url: url.into(), state, is_draft }
}

#[test]
fn status_indicator_speaks_the_hosts_terms() {
    let github = pr_status_indicator(&thread_pr(42, PrState::Open, true, "https://github.com/a/b/pull/42"), Some("github"));
    assert_eq!(
        (github.label.as_str(), github.tooltip.as_str(), github.tooltip_lead.as_str(), github.tooltip_title.as_str(), github.tone, github.glyph),
        ("PR draft", "PR #42 - Draft: Fix login", "PR #42 - Draft", "Fix login", PrTone::Draft, PrGlyph::Draft)
    );
    let gitlab = pr_status_indicator(&thread_pr(7, PrState::Merged, false, "https://gitlab.com/a/b/-/merge_requests/7"), Some("gitlab"));
    assert_eq!((gitlab.label.as_str(), gitlab.tone, gitlab.glyph), ("MR merged", PrTone::Merged, PrGlyph::Merged));
    assert_eq!(pr_status_indicator(&thread_pr(1, PrState::Closed, false, "u"), None).label, "PR closed");
    assert_eq!(pr_status_indicator(&thread_pr(1, PrState::Open, false, "u"), Some("perforce")).label, "change request open");

    let linked = link(12, json!({ "url": "https://gitlab.com/a/b/-/merge_requests/12", "snapshot": snapshot(json!({ "title": "Fix login", "state": "closed" })) }));
    let status = linked_snapshot_status(&linked).unwrap();
    assert_eq!((status.provider, status.pr.number, status.pr.state, status.pr.title.as_str()), ("gitlab", 12, PrState::Closed, "Fix login"));
    assert_eq!(linked_snapshot_status(&link(12, json!({}))), None);
}

#[test]
fn badge_presentation_covers_stacks_aggregates_singles_and_pending_status() {
    let stack_badge = ThreadPullRequestBadge::Stack { layers: 3, state: BadgeState::Merged };
    let stacked = resolve_badge_presentation(Some(&stack_badge), None, None, None).unwrap();
    assert_eq!((stacked.glyph, stacked.tone, stacked.label.as_str(), stacked.text.as_str()), (PrGlyph::Stack, PrTone::Merged, "Stack of 3 pull requests, merged", "3"));
    let gitlab_stack = resolve_badge_presentation(Some(&stack_badge), None, Some("https://gitlab.com/a/b/-/merge_requests/7"), None).unwrap();
    assert_eq!(gitlab_stack.label, "Stack of 3 merge requests, merged");

    let url = "https://github.com/a/b/pull/42";
    let status = pr_status_indicator(&thread_pr(42, PrState::Open, false, url), Some("github"));
    let others = ThreadPullRequestBadge::PullRequest { others: 2, state: BadgeState::Closed };
    let aggregate = resolve_badge_presentation(Some(&others), Some(42), Some(url), Some(&status)).unwrap();
    assert_eq!(
        (aggregate.glyph, aggregate.tone, aggregate.label.as_str(), aggregate.text.as_str()),
        (PrGlyph::Closed, PrTone::Closed, "PR #42 - Open: Fix login, and 2 more linked; overall closed", "+3")
    );
    assert!(others.opens_list() && stack_badge.opens_list());

    let alone = ThreadPullRequestBadge::PullRequest { others: 0, state: BadgeState::Open };
    assert!(!alone.opens_list());
    let shown = resolve_badge_presentation(Some(&alone), Some(42), Some(url), Some(&status)).unwrap();
    assert_eq!((shown.glyph, shown.tone, shown.label.as_str(), shown.text.as_str()), (PrGlyph::PullRequest, PrTone::Open, "PR #42 - Open: Fix login", "42"));
    let pending = resolve_badge_presentation(None, Some(42), Some(url), None).unwrap();
    assert_eq!((pending.glyph, pending.tone, pending.label.as_str()), (PrGlyph::PullRequest, PrTone::Muted, "PR #42, status pending"));
    assert_eq!(resolve_badge_presentation(Some(&alone), None, Some(url), Some(&status)), None);
}
