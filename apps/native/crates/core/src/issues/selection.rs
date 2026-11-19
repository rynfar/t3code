//! Issue label and assignee picker selection state (`packages/client-runtime/src/issueSelection.ts`).
//!
//! Included in the core issues domain foundation so `IS-DETAIL` plugs directly into it without
//! redefining selection diffing or failed-write rollback rules.

use std::collections::{BTreeMap, HashSet};

use super::wire::{IssueActor, IssueAssigneeCandidate, IssueLabel, IssueLabelCandidate};

/// A pending selection toggle keyed by item identifier (`true` = selected, `false` = deselected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionEntry<T> {
    pub selected: bool,
    pub item: T,
}

pub type SelectionDraft<T> = BTreeMap<String, SelectionEntry<T>>;

/// Partitions `draft` into items newly added vs removed compared against `is_held`.
pub fn selection_changes<T>(
    draft: &SelectionDraft<T>,
    is_held: impl Fn(&str, &T) -> bool,
) -> (Vec<&T>, Vec<&T>) {
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for (key, entry) in draft {
        let held = is_held(key, &entry.item);
        if entry.selected && !held {
            add.push(&entry.item);
        } else if !entry.selected && held {
            remove.push(&entry.item);
        }
    }
    (add, remove)
}

/// `issueLabelsAfterPicks`: computes the full replacement label list after picker toggles,
/// or `None` when the picks leave the issue unchanged.
pub fn issue_labels_after_picks(
    current: &[IssueLabel],
    draft: &SelectionDraft<IssueLabelCandidate>,
) -> Option<Vec<String>> {
    let names: Vec<String> = current.iter().map(|label| label.name.clone()).collect();
    let (add, remove) = selection_changes(draft, |name, _| names.iter().any(|held| held == name));
    if add.is_empty() && remove.is_empty() {
        return None;
    }
    let removed: HashSet<&str> = remove.iter().map(|label| label.name.as_str()).collect();
    let mut next: Vec<String> = names
        .into_iter()
        .filter(|name| !removed.contains(name.as_str()))
        .collect();
    next.extend(add.into_iter().map(|label| label.name.clone()));
    Some(next)
}

/// `issueAssigneeHeld`: case-insensitive login check.
pub fn issue_assignee_held(assignees: &[IssueActor], candidate: &IssueAssigneeCandidate) -> bool {
    assignees
        .iter()
        .any(|actor| actor.login.eq_ignore_ascii_case(&candidate.login))
}

/// `issueAssigneesAfterPicks`: people already assigned are written by login, newly picked by id.
pub fn issue_assignees_after_picks(
    current: &[IssueActor],
    draft: &SelectionDraft<IssueAssigneeCandidate>,
) -> Option<Vec<String>> {
    let (add, remove) =
        selection_changes(draft, |_, candidate| issue_assignee_held(current, candidate));
    if add.is_empty() && remove.is_empty() {
        return None;
    }
    let removed: HashSet<String> = remove
        .iter()
        .map(|candidate| candidate.login.to_lowercase())
        .collect();
    let mut next: Vec<String> = current
        .iter()
        .map(|actor| actor.login.clone())
        .filter(|login| !removed.contains(&login.to_lowercase()))
        .collect();
    next.extend(add.into_iter().map(|candidate| candidate.id.clone()));
    Some(next)
}

/// A picker's picks, the revision/version of the detail they were made against, and the set
/// last written (`IssuePicks<T>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePicks<T> {
    pub basis: Option<u64>,
    pub draft: SelectionDraft<T>,
    pub written: Option<Vec<String>>,
}

impl<T> Default for IssuePicks<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> IssuePicks<T> {
    pub fn empty() -> Self {
        Self { basis: None, draft: BTreeMap::new(), written: None }
    }
}

/// `issuePicksFor`: keeps picks while the detail revision matches; resets them on a newer read
/// unless the picker is currently open.
pub fn issue_picks_for<T: Clone>(
    picks: &IssuePicks<T>,
    detail_revision: u64,
    open: bool,
) -> IssuePicks<T> {
    if picks.basis == Some(detail_revision) {
        return picks.clone();
    }
    IssuePicks {
        basis: Some(detail_revision),
        draft: if open { picks.draft.clone() } else { BTreeMap::new() },
        written: None,
    }
}

/// `issuePicksAfterFailedWrite`: forgets a failed write and its picks unless the user changed
/// the draft since that write was sent.
pub fn issue_picks_after_failed_write<T: Clone + PartialEq>(
    latest: &IssuePicks<T>,
    sent: &IssuePicks<T>,
) -> IssuePicks<T> {
    if latest.written != sent.written {
        return latest.clone();
    }
    if latest.draft == sent.draft {
        IssuePicks::empty()
    } else {
        IssuePicks {
            basis: latest.basis,
            draft: latest.draft.clone(),
            written: None,
        }
    }
}

/// `issueWriteAfterPicks`: returns `None` when the tracker already has or was already sent the
/// target set.
pub fn issue_write_after_picks(
    after: Option<Vec<String>>,
    held: &[String],
    written: Option<&[String]>,
) -> Option<Vec<String>> {
    let target: Vec<String> = after.unwrap_or_else(|| held.to_vec());
    let sent: &[String] = written.unwrap_or(held);
    let same = target.len() == sent.len() && target.iter().all(|value| sent.contains(value));
    if same { None } else { Some(target) }
}
