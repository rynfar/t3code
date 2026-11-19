//! Issue detail presentation logic (`packages/client-runtime/src/issuePresentation.ts`,
//! `apps/web/src/components/issue/issuePresentation.tsx`, and `IssueDetailPanel.tsx` timeline).

use super::wire::{IssueComment, IssueDetail, IssueEvent, IssueEventKind, IssueState, IssueStateCategory};

/// Visual tone for an issue state (`issueStatePresentation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueStateTone {
    Open,
    Completed,
    Dismissed,
    Unknown,
}

/// Maps an `IssueState` to its semantic tone (`issueStatePresentation`).
///
/// Open and completed share the open/merged inks used by pull requests; closed states with a
/// non-`"completed"` close reason wear the muted/draft ink so cancelled or not-planned work
/// does not look finished.
pub fn issue_state_tone(state: &IssueState) -> IssueStateTone {
    match state.category {
        IssueStateCategory::Open => IssueStateTone::Open,
        IssueStateCategory::Closed => {
            if state
                .close_reason
                .as_deref()
                .is_some_and(|reason| reason != "completed")
            {
                IssueStateTone::Dismissed
            } else {
                IssueStateTone::Completed
            }
        }
        IssueStateCategory::Unknown => IssueStateTone::Unknown,
    }
}

/// `"completed"` -> `"Completed"`, `"not_planned"` -> `"Not planned"`, `"not-planned"` -> `"Not planned"`.
pub fn issue_close_reason_label(reason: &str) -> String {
    let words: Vec<&str> = reason
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .collect();
    let joined = words.join(" ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => {
            let mut out = first.to_uppercase().to_string();
            out.push_str(chars.as_str());
            out
        }
        None => String::new(),
    }
}

/// Formats an issue timeline event into its sentence (`issueEventSentence`).
pub fn issue_event_sentence(event: &IssueEvent) -> String {
    let detail = event.detail.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match event.kind {
        IssueEventKind::Closed => match detail {
            Some(reason) => format!("closed this as {}", issue_close_reason_label(reason).to_lowercase()),
            None => "closed this".to_owned(),
        },
        IssueEventKind::Reopened => "reopened this".to_owned(),
        IssueEventKind::Labeled => match detail {
            Some(label) => format!("added the {label} label"),
            None => "added a label".to_owned(),
        },
        IssueEventKind::Unlabeled => match detail {
            Some(label) => format!("removed the {label} label"),
            None => "removed a label".to_owned(),
        },
        IssueEventKind::Assigned => match detail {
            Some(person) => format!("assigned {person}"),
            None => "assigned someone".to_owned(),
        },
        IssueEventKind::Unassigned => match detail {
            Some(person) => format!("unassigned {person}"),
            None => "unassigned someone".to_owned(),
        },
        IssueEventKind::Renamed => match detail {
            Some(title) => format!("renamed this to “{title}”"),
            None => "renamed this".to_owned(),
        },
        IssueEventKind::Referenced => match detail {
            Some(source) => format!("referenced this from {source}"),
            None => "referenced this".to_owned(),
        },
        IssueEventKind::StateChanged => match detail {
            Some(state) => format!("moved this to {state}"),
            None => "changed the state".to_owned(),
        },
        IssueEventKind::Unknown => match detail {
            Some(text) => format!("updated this ({text})"),
            None => "updated this".to_owned(),
        },
    }
}

/// Resolved write permissions for an issue detail view (`issueWriteAccess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueWriteAccess {
    /// Non-`None` when writes are blocked globally (missing write scope or outside project scope).
    pub blocked: Option<String>,
    pub comment: bool,
    pub edit: bool,
    pub labels: bool,
    pub assignees: bool,
    pub react: bool,
    /// Tracker states this issue can move to (excludes the current state); empty if state changes
    /// are disallowed.
    pub states: Vec<IssueState>,
}

/// Computes what the viewer may do on `detail` (`issueWriteAccess`).
pub fn issue_write_access(detail: &IssueDetail, can_write: bool) -> IssueWriteAccess {
    let blocked = if !can_write {
        Some("Writing to this tracker needs the source-control write scope.".to_owned())
    } else if !detail.writable {
        Some("This issue is outside every project's own scope, so it is read-only here.".to_owned())
    } else {
        None
    };
    let unblocked = blocked.is_none();
    let may = |capability: bool, permission: bool| unblocked && capability && permission;
    let states = if may(true, detail.viewer_permissions.set_state) {
        detail
            .states
            .iter()
            .filter(|state| state.name != detail.state.name)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    IssueWriteAccess {
        blocked,
        comment: may(detail.capabilities.comment, detail.viewer_permissions.comment),
        edit: may(detail.capabilities.edit, detail.viewer_permissions.edit),
        labels: may(detail.capabilities.labels, detail.viewer_permissions.labels),
        assignees: may(detail.capabilities.assignees, detail.viewer_permissions.assignees),
        react: unblocked && detail.capabilities.reactions,
        states,
    }
}

/// An entry in the chronological activity timeline of an issue (`TimelineItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueTimelineItem {
    Comment(IssueComment),
    Event(IssueEvent),
}

impl IssueTimelineItem {
    pub fn created_at(&self) -> &str {
        match self {
            Self::Comment(comment) => &comment.created_at,
            Self::Event(event) => &event.created_at,
        }
    }
}

/// Merges comments and issue events chronologically oldest-first (`IssueDetailPanel.tsx`).
pub fn merge_issue_timeline(
    comments: &[IssueComment],
    events: &[IssueEvent],
) -> Vec<IssueTimelineItem> {
    let mut items: Vec<IssueTimelineItem> = Vec::with_capacity(comments.len() + events.len());
    items.extend(comments.iter().cloned().map(IssueTimelineItem::Comment));
    items.extend(events.iter().cloned().map(IssueTimelineItem::Event));
    items.sort_by(|left, right| {
        let l = crate::sidebar::time::parse_iso(left.created_at());
        let r = crate::sidebar::time::parse_iso(right.created_at());
        match (l, r) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => left.created_at().cmp(right.created_at()),
        }
    });
    items
}

/// Formats an issue author for compact preview cards (`IssuePreviewCardBody`).
pub fn issue_author_label(author: Option<&super::wire::IssueActor>) -> String {
    match author {
        None => "ghost".to_owned(),
        Some(actor) => match actor.name.as_deref() {
            Some(name) if !name.is_empty() && name != actor.login => {
                format!("{name} (@{})", actor.login)
            }
            _ => actor.login.clone(),
        },
    }
}
