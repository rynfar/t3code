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
    pub const GET_BRANCH_COMMITS: &str = "review.getBranchCommits";
}

/// `DiffPanelSelection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffSelection {
    #[serde(rename_all = "camelCase")]
    Branch { base_ref: Option<String> },
    Unstaged,
    #[serde(rename_all = "camelCase")]
    Commit { sha: String, base_ref: Option<String> },
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
    /// Preserves trailing whitespace on `file_path` so files whose names end in whitespace still
    /// reveal (`pingdotgg/t3code#37879`).
    pub fn turn(previous: &DiffSelection, turn_id: &str, file_path: Option<&str>) -> DiffSelection {
        let reveal_request_id = match previous {
            DiffSelection::Turn { reveal_request_id, .. } => reveal_request_id + 1,
            _ => 1,
        };
        DiffSelection::Turn {
            turn_id: turn_id.to_owned(),
            file_path: file_path.filter(|path| !path.is_empty()).map(str::to_owned),
            reveal_request_id,
        }
    }

    /// `selectGitScope`: the branch scope keeps the base ref it had.
    pub fn git(previous: &DiffSelection, remembered_base_ref: Option<&str>, unstaged: bool) -> DiffSelection {
        if unstaged {
            return DiffSelection::Unstaged;
        }
        let base_ref = match previous {
            DiffSelection::Branch { base_ref } | DiffSelection::Commit { base_ref, .. } => base_ref.clone(),
            _ => remembered_base_ref.map(str::to_owned),
        };
        DiffSelection::Branch { base_ref }
    }

    /// `selectCommit`: selects one branch commit while keeping the comparison base ref.
    pub fn commit(previous: &DiffSelection, remembered_base_ref: Option<&str>, sha: &str) -> DiffSelection {
        let base_ref = match previous {
            DiffSelection::Branch { base_ref } | DiffSelection::Commit { base_ref, .. } => base_ref.clone(),
            _ => remembered_base_ref.map(str::to_owned),
        };
        DiffSelection::Commit { sha: sha.to_owned(), base_ref }
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
    Commit { sha: String, base_ref: Option<String> },
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
            DiffSelection::Commit { sha, base_ref } => DiffScope::Commit { sha: sha.clone(), base_ref: base_ref.clone() },
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
            DiffScope::Commit { sha, .. } => format!("Commit {}", &sha[..sha.len().min(7)]),
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

/// `CHECKPOINT_DIFF_TRUNCATED_MARKER` (`contracts/src/checkpointDiff.ts`): ends a checkpoint diff
/// the server cut off at its output size limit.
pub const CHECKPOINT_DIFF_TRUNCATED_MARKER: &str = "\n\n[truncated]";

/// `isSelectedPatchTruncated` for a turn scope in `DiffPanel.tsx`: the server marks a cut-off
/// checkpoint diff by appending the marker instead of a flag.
pub fn is_checkpoint_diff_truncated(patch: &str) -> bool {
    patch.ends_with(CHECKPOINT_DIFF_TRUNCATED_MARKER)
}

/// `OrchestrationGetTurnDiffResult.diff` (falling back to tracked repository diffs when the
/// thread root folder itself has no Git diff).
pub fn decode_turn_diff(value: &Value) -> Result<String, String> {
    if let Some(diff) = value.get("diff").and_then(Value::as_str) {
        if !diff.is_empty() {
            return Ok(diff.to_owned());
        }
        if let Some(repos) = value.get("repositories").and_then(Value::as_array) {
            let repo_diffs: Vec<&str> = repos
                .iter()
                .filter_map(|repo| repo.get("diff").and_then(Value::as_str))
                .filter(|patch| !patch.is_empty())
                .collect();
            if !repo_diffs.is_empty() {
                return Ok(repo_diffs.join("\n"));
            }
        }
        return Ok(String::new());
    }
    if let Some(repos) = value.get("repositories").and_then(Value::as_array) {
        let repo_diffs: Vec<&str> = repos
            .iter()
            .filter_map(|repo| repo.get("diff").and_then(Value::as_str))
            .filter(|patch| !patch.is_empty())
            .collect();
        if !repo_diffs.is_empty() {
            return Ok(repo_diffs.join("\n"));
        }
    }
    Err("The server sent a diff without a patch.".to_owned())
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
    Commit,
}

impl PreviewSourceKind {
    pub fn wire(self) -> &'static str {
        match self {
            PreviewSourceKind::WorkingTree => "working-tree",
            PreviewSourceKind::BranchRange => "branch-range",
            PreviewSourceKind::Commit => "commit",
        }
    }
}

/// One `ReviewDiffPreviewSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPreviewSource {
    pub id: Option<String>,
    pub kind: PreviewSourceKind,
    pub commit_subject: Option<String>,
    pub generated_at: Option<String>,
    pub cwd: Option<String>,
    pub diff_hash: Option<String>,
    pub patches_deferred: bool,
    pub files: Vec<super::ReviewFile>,
    pub diff: String,
    pub truncated: bool,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub merge_base: Option<String>,
    /// Totals over every changed file; set when `diff` holds only some of them.
    pub totals: Option<(u64, u64)>,
}

/// One entry from `review.getBranchCommits`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCommit {
    pub sha: String,
    pub subject: String,
    pub committed_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchCommits {
    pub commits: Vec<BranchCommit>,
    pub truncated: bool,
}

pub fn branch_commits_request(cwd: &str, base_ref: Option<&str>) -> (&'static str, Value) {
    let mut payload = json!({ "cwd": cwd });
    if let Some(base_ref) = base_ref.map(str::trim).filter(|value| !value.is_empty()) {
        payload["baseRef"] = json!(base_ref);
    }
    (method::GET_BRANCH_COMMITS, payload)
}

pub fn decode_branch_commits(value: &Value) -> Result<BranchCommits, String> {
    let commits = value
        .get("commits")
        .and_then(Value::as_array)
        .ok_or_else(|| "The server sent branch commits without a commits list.".to_owned())?
        .iter()
        .filter_map(|commit| {
            Some(BranchCommit {
                sha: commit.get("sha")?.as_str()?.to_owned(),
                subject: commit.get("subject")?.as_str()?.to_owned(),
                committed_at: commit.get("committedAt")?.as_str()?.to_owned(),
            })
        })
        .collect();
    let truncated = value.get("truncated").and_then(Value::as_bool).unwrap_or(false);
    Ok(BranchCommits { commits, truncated })
}

/// A `ReviewDiffPreviewResult` with no sources at all: the cwd is not a repository
/// (`ReviewService.getDiffPreview` answers `sources: []` when no VCS is detected).
pub fn preview_is_repository(value: &Value) -> bool {
    value.get("sources").and_then(Value::as_array).is_some_and(|sources| !sources.is_empty())
}

/// `deriveWorkspaceMutationId`: changes whenever the agent may have changed the workspace, so
/// file views and host images refetch mid-turn. Commands, dynamic/MCP tools and subagents can
/// change files without reporting paths. Ports `state/threadDetail.ts`; `items` are visible in order.
pub fn workspace_mutation_id<'a>(
    items: impl DoubleEndedIterator<Item = &'a TurnItem>,
    latest_checkpoint_completed_at: Option<&str>,
) -> Option<String> {
    let item_id = items
        .rev()
        .filter(|item| matches!(item.kind, TurnItemKind::CommandExecution { .. } | TurnItemKind::FileChange { .. } | TurnItemKind::DynamicTool { .. } | TurnItemKind::Subagent { .. }))
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
    let mut files: Vec<_> = source.get("files").and_then(Value::as_array).into_iter().flatten().filter_map(|file| {
        let change_type = file.get("changeType").and_then(Value::as_str).and_then(|kind| match kind {
            "change" => Some(super::ChangeKind::Modified),
            "new" => Some(super::ChangeKind::New),
            "deleted" => Some(super::ChangeKind::Deleted),
            "rename-pure" => Some(super::ChangeKind::RenamePure),
            "rename-changed" => Some(super::ChangeKind::RenameChanged),
            _ => None,
        });
        Some(super::ReviewFile {
            path: file.get("path")?.as_str()?.to_owned(),
            previous_path: file.get("previousPath").and_then(Value::as_str).map(str::to_owned),
            change_type,
            additions: file.get("additions").and_then(Value::as_u64).unwrap_or(0),
            deletions: file.get("deletions").and_then(Value::as_u64).unwrap_or(0),
        })
    }).collect();
    files.sort_by(|a,b| crate::terminal::natural_cmp(&a.path.to_lowercase(), &b.path.to_lowercase()));
    Some(DiffPreviewSource {
        id: text("id"), kind,
        commit_subject: text("commitSubject"),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        generated_at: value.get("generatedAt").and_then(Value::as_str).map(str::to_owned),
        diff_hash: text("diffHash"),
        patches_deferred: source.get("patchesDeferred").and_then(Value::as_bool).unwrap_or(false),
        files,
        diff: text("diff").unwrap_or_default(),
        truncated: source.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        base_ref: text("baseRef"),
        head_ref: text("headRef"),
        merge_base: text("mergeBase"),
        totals,
    })
}

/// `projectCommitUrl` (`client-runtime/src/changeRequestLinks.ts` and the provider definitions).
pub fn project_commit_url(identity: Option<&Value>, sha: &str) -> Option<String> {
    let identity = identity?;
    let provider = identity.get("provider")?.as_str().filter(|v| !v.is_empty())?;
    let key = identity.get("canonicalKey")?.as_str()?;
    let (host, repository) = key.split_once('/')?;
    if host.is_empty() || repository.is_empty() || !((sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    let remote_url = identity
        .get("locator")
        .and_then(|locator| locator.get("remoteUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let web_url = identity.get("webUrl").and_then(Value::as_str).filter(|v| !v.is_empty());
    let (host, repository, route) = match provider {
        "github" => {
            let mut segments = repository.split('/');
            let (Some(owner), Some(repo), None) = (segments.next(), segments.next(), segments.next()) else { return None };
            let valid = |s: &str| !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
            if !valid(owner) || !valid(repo) { return None; }
            (host.to_owned(), repository.to_owned(), "commit")
        }
        "gitlab" => (host.to_owned(), repository.to_owned(), "-/commit"),
        "bitbucket" => (host.to_owned(), repository.to_owned(), "commits"),
        "forgejo" | "gitcafe" => (host.to_owned(), repository.to_owned(), "commit"),
        "azure-devops" => {
            let normalized = azure_canonical_key(&format!("{host}/{repository}").to_lowercase());
            let (azure_host, azure_repo) = normalized.split_once('/')?;
            (azure_host.to_owned(), azure_repo.to_owned(), "commit")
        }
        _ => return None,
    };
    let origin = remote_web_origin(&host, remote_url);
    let base = web_url.map(str::to_owned).unwrap_or_else(|| format!("{origin}/{repository}"));
    let mut url = url::Url::parse(&base).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let path = format!("{}/{route}/{}", url.path().trim_end_matches('/'), sha.to_lowercase());
    url.set_path(&path);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn remote_web_origin(host: &str, remote_url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(remote_url)
        && matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some_and(|candidate| candidate.eq_ignore_ascii_case(host))
    {
        return parsed.origin().ascii_serialization();
    }
    format!("https://{host}")
}

fn azure_canonical_key(key: &str) -> String {
    let parts: Vec<_> = key.split('/').collect();
    match parts.as_slice() {
        ["ssh.dev.azure.com" | "vs-ssh.visualstudio.com", "v3", org, project, repo] => {
            format!("dev.azure.com/{org}/{project}/_git/{repo}")
        }
        [host, tail @ ..] if host.ends_with(".visualstudio.com") => {
            let org = host.trim_end_matches(".visualstudio.com");
            let tail = tail.strip_prefix(&["defaultcollection"]).unwrap_or(tail);
            if !org.contains('.') && let [project, "_git", repo] = tail {
                format!("dev.azure.com/{org}/{project}/_git/{repo}")
            } else {
                key.to_owned()
            }
        }
        _ => key.to_owned(),
    }
}
