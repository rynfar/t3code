//! What the pull request detail panel derives from a detail and its activity, ported from
//! `apps/web/src/components/pullRequest/pullRequestDetail.logic.ts` and
//! `pullRequestPresentation.tsx`: checks summaries, the header's primary control, the merge
//! method, review outcomes, conversation threads and the timeline.

use std::collections::HashSet;

use super::wire::{Action, Activity, Actor, Check, CheckStatus, ChecksState, Comment, CommentKind, Detail, MergeMethod, Mergeability, PrState, ReviewThread, Stack};
use crate::timeline::parse_iso_ms;

/// `PULL_REQUEST_STATE_PRESENTATION` keys: an open draft reads as a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    Open,
    Draft,
    Closed,
    Merged,
}

impl StateKind {
    pub fn of(state: PrState, is_draft: bool) -> StateKind {
        match state {
            PrState::Open if is_draft => StateKind::Draft,
            PrState::Closed => StateKind::Closed,
            PrState::Merged => StateKind::Merged,
            PrState::Open | PrState::Unknown => StateKind::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StateKind::Open => "Open",
            StateKind::Draft => "Draft",
            StateKind::Closed => "Closed",
            StateKind::Merged => "Merged",
        }
    }
}

/// `classifyPullRequestChecks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksSummary {
    None,
    Pending,
    Failing,
    Passing,
}

fn failed(status: CheckStatus) -> bool {
    matches!(status, CheckStatus::Failure | CheckStatus::Cancelled)
}

pub fn classify_checks(checks: &[Check]) -> ChecksSummary {
    if checks.is_empty() {
        ChecksSummary::None
    } else if checks.iter().any(|check| failed(check.status)) {
        ChecksSummary::Failing
    } else if checks.iter().any(|check| matches!(check.status, CheckStatus::Pending | CheckStatus::ActionRequired)) {
        ChecksSummary::Pending
    } else {
        ChecksSummary::Passing
    }
}

/// `pullRequestChecksState`: null when nothing passed, failed or ran.
pub fn checks_state(checks: &[Check]) -> Option<ChecksState> {
    let statuses: HashSet<CheckStatus> = checks.iter().map(|check| check.status).collect();
    if statuses.contains(&CheckStatus::Failure) || statuses.contains(&CheckStatus::Cancelled) {
        Some(ChecksState::Failing)
    } else if statuses.contains(&CheckStatus::Pending) || statuses.contains(&CheckStatus::ActionRequired) {
        Some(ChecksState::Pending)
    } else if statuses.contains(&CheckStatus::Success) {
        Some(ChecksState::Passing)
    } else {
        None
    }
}

/// `describePullRequestChecks`.
pub fn describe_checks(checks: &[Check]) -> String {
    let total = checks.len();
    if total == 0 {
        return "No checks reported".into();
    }
    let count = |test: fn(CheckStatus) -> bool| checks.iter().filter(|check| test(check.status)).count();
    let failures = count(failed);
    let pending = count(|status| status == CheckStatus::Pending);
    let action_required = count(|status| status == CheckStatus::ActionRequired);
    let passed = count(|status| status == CheckStatus::Success);
    let mut parts = Vec::new();
    if pending > 0 {
        parts.push(format!("{pending} of {total} running"));
    }
    if action_required > 0 {
        parts.push(format!("{action_required} of {total} awaiting action"));
    }
    if failures > 0 {
        parts.push(if parts.is_empty() { format!("{failures} of {total} failing") } else { format!("{failures} failed") });
    }
    if parts.is_empty() {
        return if passed == total { "All checks passed".into() } else { format!("{passed} of {total} passing") };
    }
    parts.join(" · ")
}

/// `CHECKS_STATE_PRESENTATION` labels.
pub fn checks_state_label(state: ChecksState) -> &'static str {
    match state {
        ChecksState::Passing => "All checks have passed",
        ChecksState::Failing => "Some checks were not successful",
        ChecksState::Pending | ChecksState::Unknown => "Some checks haven't completed yet",
    }
}

/// `pullRequestCheckStatusLabel`: a workflow run awaiting approval says so.
pub fn check_status_label(check: &Check) -> &'static str {
    let workflow = check.status == CheckStatus::ActionRequired && check.url.as_deref().is_some_and(is_workflow_run_url);
    if workflow {
        return "Awaiting approval";
    }
    match check.status {
        CheckStatus::Pending => "Running",
        CheckStatus::ActionRequired => "Awaiting action",
        CheckStatus::Success => "Passed",
        CheckStatus::Failure => "Failed",
        CheckStatus::Cancelled => "Cancelled",
        CheckStatus::Skipped => "Skipped",
        CheckStatus::Neutral | CheckStatus::Unknown => "Neutral",
    }
}

/// `/\/actions\/runs\/\d+(?:\/|$)/`.
fn is_workflow_run_url(url: &str) -> bool {
    url.match_indices("/actions/runs/").any(|(index, needle)| {
        let rest = &url[index + needle.len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && rest[digits..].chars().next().is_none_or(|c| c == '/')
    })
}

/// `groupPullRequestChecks`: attention first, then running, then completed.
pub fn ordered_checks(checks: &[Check]) -> Vec<&Check> {
    let rank = |status: CheckStatus| match status {
        CheckStatus::Failure | CheckStatus::Cancelled | CheckStatus::ActionRequired => 0,
        CheckStatus::Pending => 1,
        _ => 2,
    };
    let mut ordered: Vec<&Check> = checks.iter().collect();
    ordered.sort_by_key(|check| rank(check.status));
    ordered
}

pub fn merge_method_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Rebase => "Rebase and merge",
        MergeMethod::Merge | MergeMethod::Unknown => "Merge",
    }
}

/// `resolvePullRequestMergeMethod`: the first of this pull request's pick, the project's default
/// and the last pick that the repository allows.
pub fn resolve_merge_method(allowed: &[MergeMethod], current: Option<MergeMethod>, project_default: Option<MergeMethod>, last: Option<MergeMethod>) -> MergeMethod {
    [current, project_default, last].into_iter().flatten().find(|method| allowed.contains(method)).or_else(|| allowed.first().copied()).unwrap_or(MergeMethod::Merge)
}

/// Whether merging belongs to a stack (`allowsSinglePullRequestMerge` and `stackOwnsMerge` in
/// `PullRequestDetailPanel.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeGate {
    /// A merge of this pull request alone may run.
    pub single: bool,
    /// The pull request is in a stack, so only a stack-scoped merge may run.
    pub stack_owns: bool,
    /// Why the merge control waits (`mergeBlockedReason`).
    pub blocked: Option<&'static str>,
}

/// Where the host runs stack actions, a single merge waits for `pullRequests.stack` to say the
/// pull request is in none. `stack` is `None` while that read is pending.
pub fn merge_gate(detail: &Detail, stack: Option<&Result<Option<Stack>, String>>) -> MergeGate {
    let supported = detail.capabilities.stacks == Some(true) && detail.capabilities.stack_actions == Some(true);
    if !supported {
        return MergeGate { single: true, stack_owns: false, blocked: None };
    }
    match stack {
        Some(Ok(None)) => MergeGate { single: true, stack_owns: false, blocked: None },
        Some(Ok(Some(_))) => MergeGate { single: false, stack_owns: true, blocked: Some("Checking stack membership…") },
        Some(Err(_)) => MergeGate { single: false, stack_owns: false, blocked: Some("Couldn't check stack membership") },
        None => MergeGate { single: false, stack_owns: false, blocked: Some("Checking stack membership…") },
    }
}

/// `resolvePullRequestPrimaryControl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryControl {
    Resolve,
    Ready,
    Merge,
    EnableAutoMerge,
    AutoMergeArmed,
    Merged,
    Closed,
}

pub fn primary_control(detail: &Detail, stack_owns_merge: bool) -> Option<PrimaryControl> {
    let checks = checks_state(&detail.checks);
    match detail.state {
        PrState::Merged => return Some(PrimaryControl::Merged),
        PrState::Closed => return Some(PrimaryControl::Closed),
        _ => {}
    }
    if detail.mergeability == Mergeability::Conflicting {
        return Some(PrimaryControl::Resolve);
    }
    if detail.is_draft {
        return detail.can(Action::Ready).then_some(PrimaryControl::Ready);
    }
    if detail.auto_merge_enabled == Some(true) {
        return Some(PrimaryControl::AutoMergeArmed);
    }
    if detail.allowed_merge_methods().is_empty() {
        return None;
    }
    if detail.auto_merge_enabled == Some(false) && checks.is_some_and(|state| state != ChecksState::Passing) && !stack_owns_merge && detail.can(Action::EnableAutoMerge) {
        return Some(PrimaryControl::EnableAutoMerge);
    }
    (!stack_owns_merge && detail.can(Action::Merge)).then_some(PrimaryControl::Merge)
}

/// `pullRequestReviewOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved,
    ChangesRequested,
    Dismissed,
}

impl ReviewOutcome {
    pub fn label(self) -> &'static str {
        match self {
            ReviewOutcome::Approved => "Approved",
            ReviewOutcome::ChangesRequested => "Changes requested",
            ReviewOutcome::Dismissed => "Review dismissed",
        }
    }
}

pub fn review_outcome(review_state: Option<&str>) -> Option<ReviewOutcome> {
    match review_state?.trim().to_lowercase().replace('_', "-").as_str() {
        "approved" => Some(ReviewOutcome::Approved),
        "changes-requested" => Some(ReviewOutcome::ChangesRequested),
        "dismissed" => Some(ReviewOutcome::Dismissed),
        _ => None,
    }
}

/// `visibleBody`: marker-only bodies (HTML comments) are empty.
pub fn visible_body(body: &str) -> Option<String> {
    let mut rest = body;
    let mut visible = String::new();
    while let Some(start) = rest.find("<!--") {
        visible.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                visible.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    visible.push_str(rest);
    (!visible.trim().is_empty()).then(|| body.trim().to_owned())
}

fn instant(iso: &str) -> i64 {
    parse_iso_ms(iso).unwrap_or(0)
}

/// One timeline row (`PullRequestTimelineEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    pub id: String,
    pub at: String,
    pub kind: EventKind,
    pub title: String,
    pub body: Option<String>,
    pub markdown: bool,
    pub url: Option<String>,
    pub actor: Option<Actor>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub path: Option<String>,
    pub outcome: Option<ReviewOutcome>,
    pub thread: Option<TimelineThread>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Opened,
    Commit,
    Comment,
    Review,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineThread {
    pub path: String,
    pub line: Option<u64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub replies: Vec<TimelineEvent>,
    pub more_replies: u64,
}

fn comment_event(comment: &Comment) -> TimelineEvent {
    let review = comment.kind == CommentKind::Review;
    TimelineEvent {
        id: comment.id.clone(),
        at: comment.created_at.clone(),
        kind: if review { EventKind::Review } else { EventKind::Comment },
        title: if review { "reviewed".into() } else { "commented".into() },
        body: visible_body(&comment.body),
        markdown: true,
        url: comment.url.clone(),
        actor: comment.author.clone(),
        additions: None,
        deletions: None,
        path: comment.path.clone(),
        outcome: review_outcome(comment.review_state.as_deref()),
        thread: None,
    }
}

fn thread_comment(thread: &ReviewThread, comment: &super::wire::ThreadComment) -> Comment {
    Comment {
        id: comment.id.clone(),
        kind: CommentKind::ReviewComment,
        author: comment.author.clone(),
        body: comment.body.clone(),
        created_at: comment.created_at.clone(),
        url: comment.url.clone(),
        path: Some(thread.path.clone()),
        review_state: None,
    }
}

/// `pullRequestConversationItems` folded into timeline events: each review thread becomes its
/// root comment with replies, placed at the root's time among the loose comments.
fn conversation_events(activity: &Activity) -> Vec<TimelineEvent> {
    let mut claimed = HashSet::new();
    let mut threads: Vec<TimelineEvent> = Vec::new();
    for thread in &activity.review_threads {
        let mut ordered: Vec<Comment> = thread
            .comments
            .iter()
            .map(|comment| activity.comments.iter().find(|c| c.id == comment.id).cloned().unwrap_or_else(|| thread_comment(thread, comment)))
            .collect();
        ordered.sort_by_key(|comment| instant(&comment.created_at));
        let Some((root, replies)) = ordered.split_first() else { continue };
        claimed.extend(ordered.iter().map(|comment| comment.id.clone()));
        let more = thread.comment_count.unwrap_or(ordered.len() as u64).saturating_sub(ordered.len() as u64);
        let mut event = comment_event(root);
        event.thread = Some(TimelineThread {
            path: thread.path.clone(),
            line: thread.line,
            is_resolved: thread.is_resolved,
            is_outdated: thread.is_outdated,
            replies: replies.iter().map(comment_event).collect(),
            more_replies: more,
        });
        threads.push(event);
    }
    threads.sort_by_key(|event| instant(&event.at));
    let mut items = Vec::new();
    let mut threads = threads.into_iter().peekable();
    for comment in activity.comments.iter().filter(|comment| !claimed.contains(&comment.id)) {
        while let Some(thread) = threads.next_if(|thread| instant(&thread.at) < instant(&comment.created_at)) {
            items.push(thread);
        }
        items.push(comment_event(comment));
    }
    items.extend(threads);
    items
}

/// `buildPullRequestTimeline`, newest first. Merged wins over closed: GitHub sets both.
pub fn build_timeline(detail: &Detail, activity: &Activity, term: &str) -> Vec<TimelineEvent> {
    let plain = |id: &str, at: &str, kind: EventKind, title: String, actor: Option<Actor>| TimelineEvent {
        id: id.to_owned(),
        at: at.to_owned(),
        kind,
        title,
        body: None,
        markdown: false,
        url: None,
        actor,
        additions: None,
        deletions: None,
        path: None,
        outcome: None,
        thread: None,
    };
    let capitalized = capitalize(term);
    let mut events = vec![plain("created", &detail.created_at, EventKind::Opened, format!("opened this {term}"), detail.author.clone())];
    events.extend(activity.commits.iter().map(|commit| TimelineEvent {
        body: (!commit.message_headline.is_empty()).then(|| commit.message_headline.clone()),
        additions: commit.additions,
        deletions: commit.deletions,
        ..plain(&commit.oid, &commit.committed_date, EventKind::Commit, format!("Commit {}", short_sha(&commit.oid)), commit.authors.first().cloned())
    }));
    events.extend(conversation_events(activity));
    if let Some(merged) = &detail.merged_at {
        events.push(plain("merged", merged, EventKind::Merged, format!("{capitalized} merged"), None));
    } else if let Some(closed) = &detail.closed_at {
        events.push(plain("closed", closed, EventKind::Closed, format!("{capitalized} closed"), None));
    }
    events.sort_by_key(|event| std::cmp::Reverse(instant(&event.at)));
    events
}

pub fn short_sha(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

pub fn capitalize(term: &str) -> String {
    let mut chars = term.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The host's word for a change request (`changeRequest.singular`).
pub fn change_request_term(provider: &str) -> &'static str {
    match provider {
        "gitlab" => "merge request",
        _ => "pull request",
    }
}

/// The host's name (`sourceControlClients.get(provider).label`).
pub fn host_label(provider: &str) -> &'static str {
    match provider {
        "github" => "GitHub",
        "gitlab" => "GitLab",
        "bitbucket" => "Bitbucket",
        "azure-devops" => "Azure DevOps",
        "forgejo" => "Forgejo",
        "gitcafe" => "GitCafe",
        _ => "source control",
    }
}

#[cfg(test)]
#[path = "detail_tests.rs"]
mod tests;
