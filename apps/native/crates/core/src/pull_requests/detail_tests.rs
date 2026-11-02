use serde_json::json;

use super::*;
use crate::pull_requests::wire::{Commit, ThreadComment, decode};

fn check(name: &str, status: CheckStatus) -> Check {
    Check { name: name.into(), status, description: None, url: None, required: None }
}

fn detail() -> Detail {
    decode(json!({
        "provider": "github",
        "capabilities": { "actions": ["merge", "ready", "close", "enable-auto-merge"], "mergeMethods": ["merge", "squash"], "diff": true, "comment": true },
        "viewerPermissions": { "actions": ["merge", "ready", "close", "enable-auto-merge"], "comment": true },
        "projectId": "p", "projectTitle": "App", "workspaceRoot": "/r", "repository": "acme/app", "number": 7,
        "title": "T", "body": "", "url": "https://github.com/acme/app/pull/7", "author": null,
        "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 1, "deletions": 2, "changedFiles": 1,
        "headBranch": "h", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
        "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
        "mergeCapabilities": { "merge": true, "squash": false, "rebase": true },
        "someFutureField": { "nested": true }
    }))
    .expect("detail decodes")
}

#[test]
fn describes_checks_like_the_web() {
    assert_eq!(describe_checks(&[]), "No checks reported");
    assert_eq!(describe_checks(&[check("a", CheckStatus::Success), check("b", CheckStatus::Success)]), "All checks passed");
    assert_eq!(describe_checks(&[check("a", CheckStatus::Success), check("b", CheckStatus::Skipped)]), "1 of 2 passing");
    assert_eq!(describe_checks(&[check("a", CheckStatus::Pending), check("b", CheckStatus::Failure)]), "1 of 2 running · 1 failed");
    assert_eq!(describe_checks(&[check("a", CheckStatus::Cancelled), check("b", CheckStatus::Success)]), "1 of 2 failing");
    assert_eq!(classify_checks(&[check("a", CheckStatus::ActionRequired)]), ChecksSummary::Pending);
    assert_eq!(checks_state(&[check("a", CheckStatus::Skipped)]), None);
    let checks = [check("ok", CheckStatus::Success), check("run", CheckStatus::Pending), check("bad", CheckStatus::Failure)];
    let ordered = ordered_checks(&checks);
    assert_eq!(ordered.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["bad", "run", "ok"]);
}

#[test]
fn workflow_runs_awaiting_approval_say_so() {
    let mut run = check("ci", CheckStatus::ActionRequired);
    run.url = Some("https://github.com/acme/app/actions/runs/123/job/4".into());
    assert_eq!(check_status_label(&run), "Awaiting approval");
    run.url = Some("https://github.com/acme/app/actions/runs/abc".into());
    assert_eq!(check_status_label(&run), "Awaiting action");
}

#[test]
fn primary_control_follows_state_and_permissions() {
    let mut pr = detail();
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::Merge));
    assert_eq!(pr.allowed_merge_methods(), vec![MergeMethod::Merge]);
    pr.auto_merge_enabled = Some(false);
    pr.checks = vec![check("ci", CheckStatus::Pending)];
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::EnableAutoMerge));
    pr.auto_merge_enabled = Some(true);
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::AutoMergeArmed));
    pr.is_draft = true;
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::Ready));
    pr.viewer_permissions.actions.retain(|action| *action != Action::Ready);
    assert_eq!(primary_control(&pr, false), None);
    pr.mergeability = Mergeability::Conflicting;
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::Resolve));
    pr.state = PrState::Merged;
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::Merged));
}

#[test]
fn merge_method_prefers_current_then_project_default_then_last_selected() {
    let allowed = [MergeMethod::Squash, MergeMethod::Rebase];
    assert_eq!(resolve_merge_method(&allowed, None, None, Some(MergeMethod::Rebase)), MergeMethod::Rebase);
    assert_eq!(resolve_merge_method(&allowed, None, Some(MergeMethod::Squash), Some(MergeMethod::Rebase)), MergeMethod::Squash);
    assert_eq!(resolve_merge_method(&allowed, Some(MergeMethod::Rebase), Some(MergeMethod::Squash), None), MergeMethod::Rebase);
    assert_eq!(resolve_merge_method(&allowed, None, Some(MergeMethod::Merge), Some(MergeMethod::Rebase)), MergeMethod::Rebase);
    assert_eq!(resolve_merge_method(&allowed, Some(MergeMethod::Merge), None, None), MergeMethod::Squash);
    assert_eq!(resolve_merge_method(&[], None, None, None), MergeMethod::Merge);
}

fn stack(layers: serde_json::Value) -> Stack {
    decode(json!({ "id": "s1", "number": 9, "url": "https://git.cafe/acme/app/stacks/9", "base": "main", "layers": layers })).expect("stack decodes")
}

#[test]
fn a_stack_owns_merging_where_the_host_runs_stack_actions() {
    let mut pr = detail();
    let found = Ok(Some(stack(json!([{ "number": 7, "headBranch": "h", "state": "open", "headSha": "a" }]))));
    let gate = merge_gate(&pr, None);
    assert_eq!(gate, MergeGate { single: true, stack_owns: false, blocked: None });
    assert_eq!(merge_gate(&pr, Some(&found)), gate);
    pr.capabilities.stacks = Some(true);
    assert_eq!(merge_gate(&pr, Some(&found)), gate);
    pr.capabilities.stack_actions = Some(true);
    assert_eq!(merge_gate(&pr, None), MergeGate { single: false, stack_owns: false, blocked: Some("Checking stack membership…") });
    assert_eq!(merge_gate(&pr, Some(&Err("down".into()))), MergeGate { single: false, stack_owns: false, blocked: Some("Couldn't check stack membership") });
    assert_eq!(merge_gate(&pr, Some(&Ok(None))), gate);
    let owned = merge_gate(&pr, Some(&found));
    assert!(!owned.single && owned.stack_owns);
    assert_eq!(primary_control(&pr, owned.stack_owns), None);
    assert_eq!(primary_control(&pr, false), Some(PrimaryControl::Merge));
    pr.auto_merge_enabled = Some(false);
    pr.checks = vec![check("ci", CheckStatus::Pending)];
    assert_eq!(primary_control(&pr, true), None);
}

#[test]
fn visible_body_drops_marker_only_comments() {
    assert_eq!(visible_body("<!-- bot marker -->\n "), None);
    assert_eq!(visible_body(" hi <!-- x --> "), Some("hi <!-- x -->".into()));
    assert_eq!(review_outcome(Some("CHANGES_REQUESTED")), Some(ReviewOutcome::ChangesRequested));
    assert_eq!(review_outcome(Some("commented")), None);
}

#[test]
fn timeline_threads_comments_and_puts_merges_over_closes() {
    let mut pr = detail();
    pr.merged_at = Some("2026-01-05T00:00:00Z".into());
    pr.closed_at = Some("2026-01-05T00:00:00Z".into());
    let comment = |id: &str, at: &str| Comment {
        id: id.into(),
        kind: CommentKind::IssueComment,
        author: None,
        body: "b".into(),
        created_at: at.into(),
        url: None,
        path: None,
        review_state: None,
    };
    let reply = |id: &str, at: &str| ThreadComment { id: id.into(), author: None, body: "r".into(), created_at: at.into(), url: None };
    let activity = Activity {
        comments: vec![comment("c1", "2026-01-02T00:00:00Z"), comment("c3", "2026-01-04T00:00:00Z")],
        comment_count: 2,
        comments_truncated: false,
        review_threads: vec![ReviewThread {
            id: "t".into(),
            path: "src/a.rs".into(),
            line: Some(3),
            is_resolved: true,
            is_outdated: false,
            comments: vec![reply("r2", "2026-01-03T12:00:00Z"), reply("r1", "2026-01-03T00:00:00Z")],
            comment_count: Some(4),
            next_comments_cursor: None,
        }],
        commits: vec![Commit {
            oid: "abcdef1234".into(),
            message_headline: "feat: x".into(),
            committed_date: "2026-01-01T06:00:00Z".into(),
            additions: Some(3),
            deletions: None,
            authors: Vec::new(),
        }],
    };
    let events = build_timeline(&pr, &activity, "pull request");
    let ids: Vec<&str> = events.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(ids, vec!["merged", "c3", "r1", "c1", "abcdef1234", "created"]);
    assert_eq!(events[0].title, "Pull request merged");
    assert_eq!(events[4].title, "Commit abcdef1");
    let thread = events[2].thread.as_ref().expect("thread");
    assert_eq!((thread.replies.len(), thread.more_replies), (1, 2));
    assert_eq!(events[2].path.as_deref(), Some("src/a.rs"));
}
