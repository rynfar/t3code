//! Pull request writes and what they leave on screen. Ports
//! `packages/client-runtime/src/pullRequestEditing.ts` (who may edit, the description draft's
//! lifetime, thread resolution), `pullRequestReply.ts` (`isStalePullRequestReply`) and the reply
//! and label patches in `packages/client-runtime/src/state/pullRequests.ts`.

use serde_json::{Value, json};

use super::PrRef;
use super::wire::{Detail, Label, LabelCandidate, ListEntry, PrState, Summary};

pub mod method {
    pub const UPDATE: &str = "pullRequests.update";
    pub const SET_THREAD_RESOLUTION: &str = "pullRequests.setThreadResolution";
    pub const LABEL_CANDIDATES: &str = "pullRequests.labelCandidates";
    pub const SET_LABELS: &str = "pullRequests.setLabels";
}

/// `AuthSourceControlWriteScope`, which guards every pull request write
/// (`CLIENT_GUARDED_RPC_SCOPES`).
pub const SOURCE_CONTROL_WRITE_SCOPE: &str = "source-control:write";

fn same_login(one: Option<&str>, other: Option<&str>) -> bool {
    match (one, other) {
        (Some(one), Some(other)) => one.trim().eq_ignore_ascii_case(other.trim()),
        _ => false,
    }
}

/// `canEditPullRequestChangeRequest`: the host edits change requests, and the viewer may — as
/// it reports, else as the author or someone who can merge.
pub fn can_edit_change_request(detail: &Detail) -> bool {
    if detail.capabilities.edit.as_ref().is_none_or(|edit| !edit.change_request) {
        return false;
    }
    if let Some(allowed) = detail.viewer_permissions.edit_change_request {
        return allowed;
    }
    same_login(detail.viewer.as_deref(), detail.author.as_ref().map(|author| author.login.as_str())) || detail.viewer_permissions.actions.contains(&super::wire::Action::Merge)
}

/// `canEditPullRequestMaintainerEdits`.
pub fn can_edit_maintainer_edits(detail: &Detail) -> bool {
    detail.state == PrState::Open
        && detail.capabilities.edit.as_ref().and_then(|edit| edit.maintainer_edits) == Some(true)
        && detail.viewer_permissions.maintainer_edits == Some(true)
        && detail.maintainer_can_modify.is_some()
}

/// `canEditPullRequestComment`: hosts do not support rewriting review summaries through comment mutations.
pub fn can_edit_comment(detail: &Detail, kind: super::wire::CommentKind, can_edit: Option<bool>, author: Option<&super::wire::Actor>) -> bool {
    if detail.capabilities.edit.as_ref().is_none_or(|edit| !edit.comment) {
        return false;
    }
    if !matches!(kind, super::wire::CommentKind::IssueComment | super::wire::CommentKind::ReviewComment) {
        return false;
    }
    if let Some(allowed) = can_edit {
        return allowed;
    }
    same_login(detail.viewer.as_deref(), author.map(|actor| actor.login.as_str()))
}

pub fn can_comment(detail: &Detail) -> bool {
    detail.capabilities.comment && detail.viewer_permissions.comment
}

pub fn can_reply(detail: &Detail) -> bool {
    detail.capabilities.review.reply && detail.viewer_permissions.comment
}

pub fn can_request_reviewers(detail: &Detail) -> bool {
    detail.capabilities.reviewers.request && detail.capabilities.reviewers.list_candidates
}

pub fn allowed_review_verdicts(detail: &Detail) -> Vec<super::wire::ReviewVerdict> {
    detail
        .capabilities
        .review
        .verdicts
        .iter()
        .copied()
        .filter(|verdict| *verdict != super::wire::ReviewVerdict::Unknown && detail.viewer_permissions.verdicts.contains(verdict))
        .collect()
}

/// `PullRequestUpdateInput` writing the title.
pub fn update_title_input(reference: &PrRef, title: &str) -> Value {
    let mut input = reference.payload();
    input["title"] = json!(title);
    input
}

/// `PullRequestUpdateInput` toggling `maintainerCanModify`.
pub fn update_maintainer_edits_input(reference: &PrRef, allow: bool) -> Value {
    let mut input = reference.payload();
    input["maintainerCanModify"] = json!(allow);
    input
}

/// `PullRequestResolveThreadButton`'s gate: the host resolves threads, the viewer may, and the
/// thread does not narrow that (`canResolvePullRequestThread`).
pub fn can_resolve_thread(detail: &Detail, thread_can_resolve: Option<bool>) -> bool {
    detail.capabilities.review.resolve && detail.viewer_permissions.resolve && thread_can_resolve != Some(false)
}

/// `DescriptionDraft`: a description written from this client, shown in place of the host's copy.
#[derive(Debug, Clone, PartialEq)]
pub struct DescriptionDraft {
    pub key: String,
    pub observed_at: Option<f64>,
    /// `None` once the last save failed: until a host reading, nobody knows which saves landed.
    pub body: Option<String>,
    /// Saves still running. The host applies them in order, so the last one is `body`.
    pub saves: u32,
}

/// `descriptionDraftInForce` (`maintainerEditsChoiceInForce`): a draft ends at the first host
/// reading delivered after its saves. Readers refresh once they succeed, so that reading
/// started after the write.
pub fn description_draft_in_force(draft: Option<DescriptionDraft>, key: &str, observed_at: Option<f64>) -> Option<DescriptionDraft> {
    let draft = draft.filter(|draft| draft.key == key)?;
    if draft.saves > 0 {
        return Some(DescriptionDraft { observed_at, ..draft });
    }
    (observed_at.is_some() && draft.observed_at == observed_at).then_some(draft)
}

/// `startDescriptionSave`.
pub fn start_description_save(draft: Option<DescriptionDraft>, key: &str, observed_at: Option<f64>, body: String) -> DescriptionDraft {
    let saves = draft.filter(|draft| draft.key == key).map_or(0, |draft| draft.saves);
    DescriptionDraft { key: key.to_owned(), observed_at, body: Some(body), saves: saves + 1 }
}

/// `settleDescriptionSave`: each save writes the whole body, so a failure only loses `body`
/// when no later save is running.
pub fn settle_description_save(draft: Option<DescriptionDraft>, key: &str, saved: bool) -> Option<DescriptionDraft> {
    match draft {
        Some(draft) if draft.key == key && draft.saves > 0 => {
            let saves = draft.saves - 1;
            let body = if saved || saves > 0 { draft.body } else { None };
            Some(DescriptionDraft { saves, body, ..draft })
        }
        other => other,
    }
}

/// `PullRequestUpdateInput` writing the description.
pub fn update_body_input(reference: &PrRef, body: &str) -> Value {
    let mut input = reference.payload();
    input["body"] = json!(body);
    input
}

/// `PullRequestThreadResolutionInput`.
pub fn thread_resolution_input(reference: &PrRef, thread_id: &str, resolved: bool) -> Value {
    let mut input = reference.payload();
    input["threadId"] = json!(thread_id);
    input["resolved"] = json!(resolved);
    input
}

/// `PullRequestLabelChangeInput` for one label, the way the label picker toggles them.
pub fn label_change_input(reference: &PrRef, label: &str, applied: bool) -> Value {
    let mut input = reference.payload();
    input["labels"] = json!([label]);
    input["applied"] = json!(applied);
    input
}

/// `setLabels`' `onSuccess`: the candidates and the held detail show the change at once.
pub fn apply_label_change(labels: &mut Vec<Label>, candidates: &mut [LabelCandidate], name: &str, applied: bool) {
    let color = candidates.iter().find(|candidate| candidate.name == name).and_then(|candidate| candidate.color.clone());
    for candidate in candidates.iter_mut().filter(|candidate| candidate.name == name) {
        candidate.is_applied = applied;
    }
    if applied {
        if !labels.iter().any(|label| label.name == name) {
            labels.push(Label { name: name.to_owned(), color });
        }
    } else {
        labels.retain(|label| label.name != name);
    }
}

/// What a shown pull request says about its own freshness (`ObservedPullRequest`).
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    pub state: PrState,
    pub updated_at: Option<&'a str>,
    pub observed_at: Option<f64>,
}

impl<'a> Observed<'a> {
    pub fn of_detail(detail: &'a Detail) -> Self {
        Observed { state: detail.state, updated_at: Some(&detail.updated_at), observed_at: detail.observed_at_ms() }
    }

    pub fn of_entry(entry: &'a ListEntry) -> Self {
        Observed { state: entry.state, updated_at: Some(&entry.updated_at), observed_at: entry.observed_at.as_ref().and_then(serde_json::Number::as_f64) }
    }

    pub fn of_reply(reply: &'a Summary) -> Self {
        Observed { state: reply.state, updated_at: reply.updated_at.as_deref(), observed_at: reply.observed_at }
    }
}

/// `isStalePullRequestReply`: older than what is shown by host update time, then server read
/// time, or a merge undone.
pub fn is_stale_reply(shown: Observed<'_>, reply: Observed<'_>) -> bool {
    if shown.state == PrState::Merged && reply.state != PrState::Merged {
        return true;
    }
    if let (Some(shown), Some(reply)) = (shown.updated_at.and_then(crate::sidebar::time::parse_iso), reply.updated_at.and_then(crate::sidebar::time::parse_iso))
        && shown != reply
    {
        return reply < shown;
    }
    reply.observed_at.unwrap_or(f64::NEG_INFINITY) < shown.observed_at.unwrap_or(f64::NEG_INFINITY)
}

/// The detail with an action's reply applied (`detail`'s `patch`), and whether it must still be
/// re-read for what the reply lacks (`rereads`: a reopen or a new head changes branch data and
/// permissions). `None` when the reply is stale.
pub fn patch_detail(detail: &Detail, reply: &Summary) -> Option<(Detail, bool)> {
    if is_stale_reply(Observed::of_detail(detail), Observed::of_reply(reply)) {
        return None;
    }
    let reread = (detail.state == PrState::Closed && reply.state == PrState::Open) || reply.head_sha.as_ref().is_some_and(|sha| detail.head_sha.as_ref() != Some(sha));
    let mut next = detail.clone();
    next.state = reply.state;
    if let Some(title) = &reply.title {
        next.title = title.clone();
    }
    if let Some(url) = &reply.url {
        next.url = url.clone();
    }
    if let Some(author) = &reply.author {
        next.author = author.clone();
    }
    if let Some(is_draft) = reply.is_draft {
        next.is_draft = is_draft;
    }
    if let Some(mergeability) = reply.mergeability {
        next.mergeability = mergeability;
    }
    next.additions = reply.additions.unwrap_or(next.additions);
    next.deletions = reply.deletions.unwrap_or(next.deletions);
    next.changed_files = reply.changed_files.unwrap_or(next.changed_files);
    if let Some(branch) = &reply.head_branch {
        next.head_branch = branch.clone();
    }
    if let Some(branch) = &reply.base_branch {
        next.base_branch = branch.clone();
    }
    if let Some(updated) = &reply.updated_at {
        next.updated_at = updated.clone();
    }
    if let Some(merged) = &reply.merged_at {
        next.merged_at = merged.clone();
    }
    if let Some(closed) = &reply.closed_at {
        next.closed_at = closed.clone();
    }
    if reply.auto_merge_enabled.is_some() {
        next.auto_merge_enabled = reply.auto_merge_enabled;
    }
    if reply.auto_merge_method.is_some() {
        next.auto_merge_method = reply.auto_merge_method;
    }
    if reply.auto_merge_enabled == Some(false) {
        next.auto_merge_method = None;
    }
    Some((next, reread))
}

/// `patchPullRequestListEntries` for one listing row: the reply applied unless stale. `false`
/// when the row no longer belongs in a listing of `state`/`draft` and should drop out.
pub fn patch_list_entry(entry: &mut ListEntry, reply: &Summary, accepts: impl Fn(PrState, bool) -> bool) -> bool {
    if is_stale_reply(Observed::of_entry(entry), Observed::of_reply(reply)) {
        return true;
    }
    entry.state = reply.state;
    if let Some(title) = &reply.title {
        entry.title = title.clone();
    }
    if let Some(url) = &reply.url {
        entry.url = url.clone();
    }
    if let Some(author) = &reply.author {
        entry.author = author.clone();
    }
    if let Some(branch) = &reply.head_branch {
        entry.head_branch = branch.clone();
    }
    if let Some(branch) = &reply.base_branch {
        entry.base_branch = branch.clone();
    }
    if let Some(is_draft) = reply.is_draft {
        entry.is_draft = is_draft;
    }
    if let Some(mergeability) = reply.mergeability {
        entry.mergeability = mergeability;
    }
    if let Some(updated) = &reply.updated_at {
        entry.updated_at = updated.clone();
    }
    accepts(entry.state, entry.is_draft)
}

#[cfg(test)]
#[path = "editing_tests.rs"]
mod tests;
