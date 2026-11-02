//! What the diff panel shows: the per-thread selection (`diffPanelStore.ts`), the turns that have
//! checkpoints (`deriveThreadCheckpointSummaries`), and the requests behind each scope
//! (`useCheckpointDiff` in `state/queries.ts`, `review.getDiffPreview`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ToolCallTarget;
use crate::thread::Entity;
use crate::turn_item::{TurnItem, TurnItemKind};

pub mod method {
    pub const GET_TURN_DIFF: &str = "orchestration.getTurnDiff";
    pub const GET_FULL_THREAD_DIFF: &str = "orchestration.getFullThreadDiff";
    pub const GET_DIFF_PREVIEW: &str = "review.getDiffPreview";
}

/// `DiffPanelSelection` without the commit kind, which native does not offer yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffSelection {
    #[serde(rename_all = "camelCase")]
    Branch { base_ref: Option<String> },
    Unstaged,
    #[serde(rename_all = "camelCase")]
    Turn { turn_id: String, file_path: Option<String>, reveal_request_id: u64 },
    /// One file-change tool call's recorded patch (`selectToolCall`).
    #[serde(rename = "tool-call")]
    ToolCall(ToolCallTarget),
}

impl Default for DiffSelection {
    fn default() -> Self {
        DiffSelection::Branch { base_ref: None }
    }
}

impl DiffSelection {
    /// `selectTurn`: each request to reveal a file counts up, so repeating it reveals again.
    pub fn turn(previous: &DiffSelection, turn_id: &str, file_path: Option<&str>) -> DiffSelection {
        let reveal_request_id = match previous {
            DiffSelection::Turn { reveal_request_id, .. } => reveal_request_id + 1,
            _ => 1,
        };
        DiffSelection::Turn {
            turn_id: turn_id.to_owned(),
            file_path: file_path.map(str::trim).filter(|path| !path.is_empty()).map(str::to_owned),
            reveal_request_id,
        }
    }

    /// `selectGitScope`: the branch scope keeps the base ref it had.
    pub fn git(previous: &DiffSelection, remembered_base_ref: Option<&str>, unstaged: bool) -> DiffSelection {
        if unstaged {
            return DiffSelection::Unstaged;
        }
        let base_ref = match previous {
            DiffSelection::Branch { base_ref } => base_ref.clone(),
            _ => remembered_base_ref.map(str::to_owned),
        };
        DiffSelection::Branch { base_ref }
    }

    /// `reconcileTurnSelection`: a turn that disappeared (rolled back) falls to the latest.
    /// `ordered` is newest first. Returns true when the selection changed.
    pub fn reconcile(&mut self, ordered: &[TurnDiffSummary]) -> bool {
        let DiffSelection::Turn { turn_id, .. } = self else { return false };
        let Some(latest) = ordered.first() else { return false };
        if ordered.iter().any(|summary| summary.run_id == *turn_id) {
            return false;
        }
        *turn_id = latest.run_id.clone();
        true
    }
}

/// The scope a selection resolves to once the thread's turns are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffScope {
    Branch { base_ref: Option<String> },
    Unstaged,
    Turn { summary_index: usize, latest: bool },
    /// A turn selection on a thread with no completed turns.
    NoTurns,
    ToolCall(ToolCallTarget),
}

impl DiffScope {
    pub fn resolve(selection: &DiffSelection, ordered: &[TurnDiffSummary]) -> DiffScope {
        match selection {
            DiffSelection::Branch { base_ref } => DiffScope::Branch { base_ref: base_ref.clone() },
            DiffSelection::Unstaged => DiffScope::Unstaged,
            DiffSelection::Turn { turn_id, .. } => {
                if ordered.is_empty() {
                    return DiffScope::NoTurns;
                }
                let index = ordered.iter().position(|summary| summary.run_id == *turn_id).unwrap_or(0);
                DiffScope::Turn { summary_index: index, latest: index == 0 }
            }
            DiffSelection::ToolCall(target) => DiffScope::ToolCall(target.clone()),
        }
    }

    /// The scope trigger's label in `DiffPanel.tsx`.
    pub fn label(&self, ordered: &[TurnDiffSummary]) -> String {
        match self {
            DiffScope::Branch { .. } => "Changes".to_owned(),
            DiffScope::Unstaged => "Uncommitted".to_owned(),
            DiffScope::Turn { latest: true, .. } => "Latest turn".to_owned(),
            DiffScope::Turn { summary_index, .. } => format!("Turn {}", ordered[*summary_index].turn_count),
            DiffScope::NoTurns => "Latest turn".to_owned(),
            DiffScope::ToolCall(_) => "Tool call".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CheckpointFile {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct FileTotals {
    pub count: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// `ThreadCheckpointSummary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDiffSummary {
    pub checkpoint_id: String,
    pub run_id: String,
    /// The checkpoint's `appRunOrdinal`: turn N spans checkpoints N-1..N.
    pub turn_count: u64,
    pub status: String,
    pub files: Vec<CheckpointFile>,
    pub files_total: Option<FileTotals>,
    pub completed_at: String,
}

impl TurnDiffSummary {
    /// `checkpointFileTotals`: `filesTotal` when the wire sent a prefix of the files.
    pub fn totals(&self) -> FileTotals {
        self.files_total.unwrap_or_else(|| FileTotals {
            count: self.files.len() as u64,
            additions: self.files.iter().map(|file| file.additions).sum(),
            deletions: self.files.iter().map(|file| file.deletions).sum(),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheckpoint {
    id: String,
    run_id: Option<String>,
    app_run_ordinal: Option<u64>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    files: Vec<CheckpointFile>,
    #[serde(default)]
    files_total: Option<FileTotals>,
    #[serde(default)]
    captured_at: String,
}

/// Turns with an app checkpoint, newest first (the order `DiffPanel.tsx` sorts them in).
/// Checkpoints of subagents and tools have no `appRunOrdinal` and are skipped.
pub fn turn_diff_summaries(checkpoints: &[Entity]) -> Vec<TurnDiffSummary> {
    let mut summaries: Vec<TurnDiffSummary> = checkpoints
        .iter()
        .filter_map(|entity| serde_json::from_value::<RawCheckpoint>(entity.value.clone()).ok())
        .filter_map(|raw| {
            Some(TurnDiffSummary {
                checkpoint_id: raw.id,
                run_id: raw.run_id?,
                turn_count: raw.app_run_ordinal?,
                status: raw.status,
                files: raw.files,
                files_total: raw.files_total,
                completed_at: raw.captured_at,
            })
        })
        .collect();
    summaries.sort_by(|left, right| {
        right.turn_count.cmp(&left.turn_count).then_with(|| right.completed_at.cmp(&left.completed_at))
    });
    summaries
}

/// `useCheckpointDiff`: turn N diffs checkpoints N-1..N; from zero that is the full-thread diff.
pub fn checkpoint_diff_request(thread_id: &str, turn_count: u64, ignore_whitespace: bool) -> (&'static str, Value) {
    let from = turn_count.saturating_sub(1);
    if from == 0 {
        let payload = json!({ "threadId": thread_id, "toTurnCount": turn_count, "ignoreWhitespace": ignore_whitespace });
        (method::GET_FULL_THREAD_DIFF, payload)
    } else {
        let payload = json!({
            "threadId": thread_id,
            "fromTurnCount": from,
            "toTurnCount": turn_count,
            "ignoreWhitespace": ignore_whitespace,
        });
        (method::GET_TURN_DIFF, payload)
    }
}

/// `OrchestrationGetTurnDiffResult.diff`.
pub fn decode_turn_diff(value: &Value) -> Result<String, String> {
    value
        .get("diff")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "The server sent a diff without a patch.".to_owned())
}

pub fn diff_preview_request(cwd: &str, base_ref: Option<&str>, ignore_whitespace: bool) -> (&'static str, Value) {
    let mut payload = json!({ "cwd": cwd, "ignoreWhitespace": ignore_whitespace });
    if let Some(base_ref) = base_ref {
        payload["baseRef"] = json!(base_ref);
    }
    (method::GET_DIFF_PREVIEW, payload)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSourceKind {
    WorkingTree,
    BranchRange,
}

impl PreviewSourceKind {
    fn wire(self) -> &'static str {
        match self {
            PreviewSourceKind::WorkingTree => "working-tree",
            PreviewSourceKind::BranchRange => "branch-range",
        }
    }
}

/// One `ReviewDiffPreviewSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPreviewSource {
    pub diff: String,
    pub truncated: bool,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    /// Totals over every changed file; set when `diff` holds only some of them.
    pub totals: Option<(u64, u64)>,
}

/// A `ReviewDiffPreviewResult` with no sources at all: the cwd is not a repository
/// (`ReviewService.getDiffPreview` answers `sources: []` when no VCS is detected).
pub fn preview_is_repository(value: &Value) -> bool {
    value.get("sources").and_then(Value::as_array).is_some_and(|sources| !sources.is_empty())
}

/// `deriveWorkspaceMutationId`: changes whenever the agent may have changed the workspace, so
/// git diffs refetch mid-turn. File tools are explicit; finished commands count because a shell
/// command can change files without reporting them. `items` are the visible turn items in order.
pub fn workspace_mutation_id<'a>(
    items: impl DoubleEndedIterator<Item = &'a TurnItem>,
    latest_checkpoint_completed_at: Option<&str>,
) -> Option<String> {
    let item_id = items
        .rev()
        .filter(|item| matches!(item.kind, TurnItemKind::CommandExecution { .. } | TurnItemKind::FileChange { .. }))
        .find(|item| !matches!(item.status.as_str(), "pending" | "running" | "waiting"))
        .map(|item| item.id.as_str());
    if item_id.is_none() && latest_checkpoint_completed_at.is_none() {
        return None;
    }
    Some(json!([item_id, latest_checkpoint_completed_at]).to_string())
}

/// Picks the source for `kind` from a `ReviewDiffPreviewResult`.
pub fn decode_diff_preview(value: &Value, kind: PreviewSourceKind) -> Option<DiffPreviewSource> {
    let source = value.get("sources")?.as_array()?.iter().find(|source| source.get("kind").and_then(Value::as_str) == Some(kind.wire()))?;
    let text = |key: &str| source.get(key).and_then(Value::as_str).map(str::to_owned);
    let totals = source.get("files").and_then(Value::as_array).map(|files| {
        files.iter().fold((0, 0), |(adds, dels), file| {
            (
                adds + file.get("additions").and_then(Value::as_f64).unwrap_or(0.) as u64,
                dels + file.get("deletions").and_then(Value::as_f64).unwrap_or(0.) as u64,
            )
        })
    });
    Some(DiffPreviewSource {
        diff: text("diff").unwrap_or_default(),
        truncated: source.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        base_ref: text("baseRef"),
        head_ref: text("headRef"),
        totals,
    })
}
