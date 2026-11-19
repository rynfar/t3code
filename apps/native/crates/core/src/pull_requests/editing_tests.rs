use serde_json::json;

use super::*;
use crate::pull_requests::wire::{LabelCandidate, decode};

fn detail(overrides: Value) -> Detail {
    let mut value = json!({
        "provider": "github",
        "capabilities": { "actions": ["merge", "close"], "mergeMethods": ["merge"], "edit": { "changeRequest": true, "comment": true }, "review": { "resolve": true } },
        "viewerPermissions": { "actions": ["close"], "comment": true, "resolve": true },
        "projectId": "p", "projectTitle": "App", "workspaceRoot": "/r", "repository": "acme/app", "number": 7,
        "title": "T", "body": "", "url": "https://github.com/acme/app/pull/7", "author": { "login": "octocat" },
        "viewer": "octocat", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 1, "deletions": 2, "changedFiles": 1,
        "headBranch": "h", "headSha": "a1", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
        "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
        "mergeCapabilities": { "merge": true, "squash": false, "rebase": false },
        "mergeQueueRemoval": { "id": "q1", "removedAt": "2026-01-02T00:00:00Z", "reason": "checks failed" }
    });
    if let (Some(target), Some(overrides)) = (value.as_object_mut(), overrides.as_object()) {
        for (key, field) in overrides {
            target.insert(key.clone(), field.clone());
        }
    }
    decode(value).expect("detail decodes")
}

#[test]
fn editing_a_change_request_follows_the_host_then_the_author_or_a_merger() {
    assert!(can_edit_change_request(&detail(json!({}))));
    assert!(can_edit_change_request(&detail(json!({ "viewer": "OCTOCAT" }))), "logins match regardless of case");
    assert!(!can_edit_change_request(&detail(json!({ "author": { "login": "someone-else" } }))));
    assert!(!can_edit_change_request(&detail(json!({ "viewerPermissions": { "actions": [], "comment": true, "resolve": true, "editChangeRequest": false } }))));
    assert!(can_edit_change_request(&detail(json!({ "author": { "login": "someone-else" }, "viewerPermissions": { "actions": [], "comment": true, "resolve": true, "editChangeRequest": true } }))));
    // Where the host cannot rewrite a change request at all, nobody can.
    assert!(!can_edit_change_request(&detail(json!({ "capabilities": { "actions": [], "mergeMethods": [], "edit": { "changeRequest": false, "comment": true } } }))));
    assert!(!can_edit_change_request(&detail(json!({ "capabilities": { "actions": [], "mergeMethods": [] } }))));
    // A merger the host could not name still may.
    assert!(can_edit_change_request(&detail(json!({ "viewer": null, "viewerPermissions": { "actions": ["merge"], "comment": true, "resolve": true } }))));
}

#[test]
fn resolving_a_thread_narrows_the_repository_permission_per_thread() {
    let open = detail(json!({}));
    assert!(can_resolve_thread(&open, None));
    assert!(!can_resolve_thread(&open, Some(false)));
    let refused = detail(json!({ "viewerPermissions": { "actions": [], "comment": true, "resolve": false } }));
    assert!(!can_resolve_thread(&refused, None));
    assert!(!can_resolve_thread(&refused, Some(true)));
    let unsupported = detail(json!({ "capabilities": { "actions": [], "mergeMethods": [], "review": { "resolve": false } } }));
    assert!(!can_resolve_thread(&unsupported, None));
}

const URL: &str = "https://github.com/owner/repo/pull/1";
const UNTICKED: &str = "- [ ] Ship it";
const TICKED: &str = "- [x] Ship it";

#[test]
fn description_drafts_hide_readings_during_saves_and_end_at_the_first_after_them() {
    // Ticking and unticking the same box saves the host's own body last.
    let draft = start_description_save(None, URL, Some(1.), TICKED.into());
    let draft = start_description_save(Some(draft), URL, Some(1.), UNTICKED.into());
    let draft = description_draft_in_force(Some(draft), URL, Some(2.));
    assert_eq!(draft.as_ref().and_then(|d| d.body.as_deref()), Some(UNTICKED));
    let draft = settle_description_save(draft, URL, true);
    assert_eq!(description_draft_in_force(draft.clone(), URL, Some(2.)).and_then(|d| d.body), Some(UNTICKED.into()));
    let draft = settle_description_save(draft, URL, true);
    assert_eq!(description_draft_in_force(draft.clone(), URL, Some(2.)).and_then(|d| d.body), Some(UNTICKED.into()));
    // The acknowledging reading ends the draft, so the host's later ticks show.
    assert_eq!(description_draft_in_force(draft, URL, Some(3.)), None);
}

#[test]
fn description_drafts_outlive_failed_saves_losing_the_body_only_with_the_last() {
    let draft = start_description_save(None, URL, Some(1.), TICKED.into());
    let draft = start_description_save(Some(draft), URL, Some(1.), UNTICKED.into());
    // The save still running writes the whole body again.
    let draft = settle_description_save(Some(draft), URL, false);
    let draft = description_draft_in_force(draft, URL, Some(2.));
    assert_eq!(draft.as_ref().map(|d| (d.body.as_deref(), d.saves)), Some((Some(UNTICKED), 1)));
    let draft = settle_description_save(draft, URL, false);
    let kept = description_draft_in_force(draft.clone(), URL, Some(2.));
    assert_eq!(kept.as_ref().map(|d| (d.body.as_deref(), d.saves)), Some((None, 0)));
    assert_eq!(settle_description_save(draft.clone(), URL, true), draft);
    assert_eq!(description_draft_in_force(draft, URL, Some(3.)), None);
    // A server without reading times cannot acknowledge, so the draft ends with its saves.
    let unstamped = settle_description_save(Some(start_description_save(None, URL, None, TICKED.into())), URL, true);
    assert_eq!(description_draft_in_force(unstamped, URL, None), None);
}

#[test]
fn description_drafts_belong_to_one_change_request() {
    let draft = start_description_save(None, URL, Some(1.), TICKED.into());
    let other = "https://github.com/owner/repo/pull/2";
    assert_eq!(description_draft_in_force(Some(draft.clone()), other, Some(1.)), None);
    assert_eq!(settle_description_save(Some(draft.clone()), other, false), Some(draft.clone()));
    assert_eq!(start_description_save(Some(draft), other, Some(5.), UNTICKED.into()).saves, 1);
}

#[test]
fn writes_name_the_pull_request() {
    let reference = PrRef { project_id: "p".into(), host: Some("github.com".into()), repository: "a/b".into(), number: 3 };
    assert_eq!(update_body_input(&reference, "- [x] a"), json!({ "projectId": "p", "host": "github.com", "repository": "a/b", "number": 3, "body": "- [x] a" }));
    assert_eq!(thread_resolution_input(&reference, "t1", true)["resolved"], true);
    assert_eq!(label_change_input(&reference, "bug", false), json!({ "projectId": "p", "host": "github.com", "repository": "a/b", "number": 3, "labels": ["bug"], "applied": false }));
}

#[test]
fn label_changes_show_on_the_candidates_and_the_detail() {
    let candidate = |name: &str, applied: bool| LabelCandidate { name: name.into(), color: Some("ff0000".into()), description: None, is_applied: applied };
    let mut labels = vec![Label { name: "ui".into(), color: None }];
    let mut candidates = vec![candidate("bug", false), candidate("ui", true)];
    apply_label_change(&mut labels, &mut candidates, "bug", true);
    assert_eq!(labels.iter().map(|l| (l.name.as_str(), l.color.as_deref())).collect::<Vec<_>>(), vec![("ui", None), ("bug", Some("ff0000"))]);
    assert!(candidates[0].is_applied);
    apply_label_change(&mut labels, &mut candidates, "bug", true);
    assert_eq!(labels.len(), 2, "applying twice adds once");
    apply_label_change(&mut labels, &mut candidates, "ui", false);
    assert_eq!(labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["bug"]);
    assert!(!candidates[1].is_applied);
}

fn reply(value: Value) -> Summary {
    serde_json::from_value(value).expect("summary decodes")
}

#[test]
fn stale_replies_lose_to_what_is_shown() {
    let shown = detail(json!({}));
    // Older by host time, even when read later.
    assert!(is_stale_reply(Observed::of_detail(&shown), Observed::of_reply(&reply(json!({ "state": "closed", "updatedAt": "2026-01-01T00:00:00Z", "observedAt": 500 })))));
    // Same host time: the server's read time decides.
    assert!(is_stale_reply(Observed::of_detail(&shown), Observed::of_reply(&reply(json!({ "state": "closed", "updatedAt": "2026-01-02T00:00:00Z", "observedAt": 50 })))));
    assert!(!is_stale_reply(Observed::of_detail(&shown), Observed::of_reply(&reply(json!({ "state": "closed", "updatedAt": "2026-01-03T00:00:00Z" })))));
    // A merge is never undone by a reply.
    let merged = detail(json!({ "state": "merged" }));
    assert!(is_stale_reply(Observed::of_detail(&merged), Observed::of_reply(&reply(json!({ "state": "open", "updatedAt": "2026-02-01T00:00:00Z" })))));
}

#[test]
fn action_replies_patch_the_detail_and_say_when_a_read_must_follow() {
    let shown = detail(json!({ "autoMergeEnabled": true, "autoMergeMethod": "squash" }));
    let (closed, reread) = patch_detail(&shown, &reply(json!({ "state": "closed", "updatedAt": "2026-01-03T00:00:00Z", "closedAt": "2026-01-03T00:00:00Z", "autoMergeEnabled": false }))).unwrap();
    assert_eq!((closed.state, closed.closed_at.as_deref(), closed.auto_merge_enabled, closed.auto_merge_method, reread), (PrState::Closed, Some("2026-01-03T00:00:00Z"), Some(false), None, false));
    assert_eq!(closed.title, "T", "fields the reply lacks stay");
    // Reopening changes branch data and permissions the reply lacks.
    let (_, reread) = patch_detail(&closed, &reply(json!({ "state": "open", "updatedAt": "2026-01-04T00:00:00Z", "closedAt": null }))).unwrap();
    assert!(reread);
    // So does a new head.
    let (ready, reread) = patch_detail(&shown, &reply(json!({ "state": "open", "isDraft": false, "updatedAt": "2026-01-03T00:00:00Z", "headSha": "b2" }))).unwrap();
    assert!(reread && !ready.is_draft);
    assert!(patch_detail(&shown, &reply(json!({ "state": "closed", "updatedAt": "2026-01-01T00:00:00Z" }))).is_none());
}

#[test]
fn action_replies_patch_listing_rows_and_drop_ones_the_listing_no_longer_takes() {
    let mut entry: ListEntry = decode(json!({
        "provider": "github", "host": "github.com", "projectId": "p", "projectTitle": "P", "repository": "a/b", "number": 1,
        "title": "t", "url": "u", "author": null, "headBranch": "h", "baseBranch": "main", "state": "open", "isDraft": true,
        "mergeability": "mergeable", "additions": 0, "deletions": 0, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
        "viewerReviewRequested": false, "labels": []
    }))
    .unwrap();
    let open_only = |state: PrState, _: bool| state == PrState::Open;
    assert!(patch_list_entry(&mut entry, &reply(json!({ "state": "open", "isDraft": false, "updatedAt": "2026-01-02T00:00:00Z" })), open_only));
    assert_eq!((entry.is_draft, entry.updated_at.as_str()), (false, "2026-01-02T00:00:00Z"));
    assert!(!patch_list_entry(&mut entry, &reply(json!({ "state": "closed", "updatedAt": "2026-01-03T00:00:00Z" })), open_only));
    // A stale reply leaves the row as it was.
    let mut kept = entry.clone();
    assert!(patch_list_entry(&mut kept, &reply(json!({ "state": "merged", "updatedAt": "2025-01-01T00:00:00Z" })), open_only));
    assert_eq!(kept, entry);
}

#[test]
fn merge_queue_removals_reading_times_and_replies_decode() {
    let shown = detail(json!({}));
    assert_eq!(shown.merge_queue_removal.as_ref().and_then(|removal| removal.reason.as_deref()), Some("checks failed"));
    assert_eq!(shown.observed_at_ms(), Some(100.));
    assert_eq!(decode_summary_reply(Value::Null), None);
    assert_eq!(decode_summary_reply(json!({ "state": "merged" })).map(|summary| summary.state), Some(PrState::Merged));
}

fn decode_summary_reply(value: Value) -> Option<Summary> {
    crate::pull_requests::wire::decode_summary(value).unwrap()
}
