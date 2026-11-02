//! Source control status, refs and the branch toolbar's rules. Ports the status and ref shapes in
//! `packages/contracts/src/git.ts`, `applyGitStatusStreamEvent` and the ref-name helpers in
//! `packages/shared/src/git.ts`, `apps/web/src/components/BranchToolbar.logic.ts`, the branch
//! picker's badges and filter (`BranchPicker.tsx`), the branch permissions in
//! `BranchToolbarBranchSelector.tsx`, and `resolveChangesTotals` (`GitActionsControl.logic.ts`).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auth::{AuthSessionState, ORCHESTRATION_OPERATE_SCOPE, SOURCE_CONTROL_WRITE_SCOPE};

pub mod method {
    pub const SUBSCRIBE_VCS_STATUS: &str = "subscribeVcsStatus";
    pub const VCS_PULL: &str = "vcs.pull";
    pub const VCS_REFRESH_STATUS: &str = "vcs.refreshStatus";
    pub const VCS_LIST_REFS: &str = "vcs.listRefs";
    pub const VCS_CREATE_REF: &str = "vcs.createRef";
    pub const VCS_SWITCH_REF: &str = "vcs.switchRef";
    pub const VCS_INIT: &str = "vcs.init";
    pub const GIT_RUN_STACKED_ACTION: &str = "git.runStackedAction";
    pub const SHELL_OPEN_IN_EDITOR: &str = "shell.openInEditor";
    pub const TERMINAL_OPEN: &str = "terminal.open";
    pub const TERMINAL_WRITE: &str = "terminal.write";
    pub const SUBSCRIBE_TERMINAL_METADATA: &str = "subscribeTerminalMetadata";
}

/// `queries.ts` `VCS_REF_LIST_LIMIT`: one page of the branch picker.
pub const REF_PAGE_LIMIT: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProvider {
    pub kind: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTree {
    /// The worktree root every `files[].path` is relative to; absent when the server could not
    /// resolve it.
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub files: Vec<ChangedFile>,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchChanges {
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub base_ref: String,
    #[serde(default)]
    pub head_ref: String,
    /// `open`, `closed` or `merged`.
    pub state: String,
    #[serde(default)]
    pub is_draft: Option<bool>,
}

/// `VcsStatusLocalResult`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsLocalStatus {
    pub is_repo: bool,
    #[serde(default)]
    pub source_control_provider: Option<SourceControlProvider>,
    #[serde(default)]
    pub has_primary_remote: bool,
    #[serde(default)]
    pub is_default_ref: bool,
    #[serde(default)]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub has_working_tree_changes: bool,
    #[serde(default)]
    pub working_tree: WorkingTree,
    #[serde(default)]
    pub branch_changes: Option<BranchChanges>,
}

impl Default for VcsLocalStatus {
    /// The local part `applyGitStatusStreamEvent` assumes when remote status arrives first.
    fn default() -> Self {
        VcsLocalStatus {
            is_repo: true,
            source_control_provider: None,
            has_primary_remote: false,
            is_default_ref: false,
            ref_name: None,
            has_working_tree_changes: false,
            working_tree: WorkingTree::default(),
            branch_changes: None,
        }
    }
}

/// `VcsStatusRemoteResult`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRemoteStatus {
    #[serde(default)]
    pub has_upstream: bool,
    #[serde(default)]
    pub ahead_count: u64,
    #[serde(default)]
    pub behind_count: u64,
    #[serde(default)]
    pub ahead_of_default_count: Option<u64>,
    #[serde(default)]
    pub pr: Option<ChangeRequest>,
}

impl Default for VcsRemoteStatus {
    /// `EMPTY_GIT_STATUS_REMOTE`.
    fn default() -> Self {
        VcsRemoteStatus { has_upstream: false, ahead_count: 0, behind_count: 0, ahead_of_default_count: Some(0), pr: None }
    }
}

/// `VcsStatusResult`: the local and remote halves the stream updates separately.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct VcsStatus {
    #[serde(flatten)]
    pub local: VcsLocalStatus,
    #[serde(flatten)]
    pub remote: VcsRemoteStatus,
}

impl VcsStatus {
    pub fn has_open_pr(&self) -> bool {
        self.remote.pr.as_ref().is_some_and(|pr| pr.state == "open")
    }

    pub fn provider_kind(&self) -> Option<&str> {
        self.local.source_control_provider.as_ref().map(|provider| provider.kind.as_str())
    }
}

/// `VcsStatusStreamEvent`, one chunk of `subscribeVcsStatus`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "_tag")]
pub enum VcsStatusEvent {
    #[serde(rename = "snapshot")]
    Snapshot { local: VcsLocalStatus, remote: Option<VcsRemoteStatus> },
    #[serde(rename = "localUpdated")]
    LocalUpdated { local: VcsLocalStatus },
    #[serde(rename = "remoteUpdated")]
    RemoteUpdated { remote: Option<VcsRemoteStatus> },
    #[serde(other)]
    Unknown,
}

/// `applyGitStatusStreamEvent`. Unknown events leave the status as it was.
pub fn apply_status_event(current: Option<VcsStatus>, event: VcsStatusEvent) -> Option<VcsStatus> {
    match event {
        VcsStatusEvent::Snapshot { local, remote } => Some(VcsStatus { local, remote: remote.unwrap_or_default() }),
        VcsStatusEvent::LocalUpdated { local } => Some(VcsStatus { local, remote: current.map(|status| status.remote).unwrap_or_default() }),
        VcsStatusEvent::RemoteUpdated { remote } => {
            Some(VcsStatus { local: current.map(|status| status.local).unwrap_or_default(), remote: remote.unwrap_or_default() })
        }
        VcsStatusEvent::Unknown => current,
    }
}

/// `resolveChangesTotals`: branch totals unless they are zero while the tree is dirty.
pub fn changes_totals(status: &VcsStatus) -> (u64, u64) {
    let tree = &status.local.working_tree;
    match &status.local.branch_changes {
        Some(branch) if branch.insertions > 0 || branch.deletions > 0 => (branch.insertions, branch.deletions),
        _ if tree.insertions > 0 || tree.deletions > 0 => (tree.insertions, tree.deletions),
        Some(branch) => (branch.insertions, branch.deletions),
        None => (tree.insertions, tree.deletions),
    }
}

/// `formatDiffCount`: grouped with commas, as `Intl.NumberFormat` does in English.
pub fn format_diff_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRef {
    pub name: String,
    #[serde(default)]
    pub is_remote: Option<bool>,
    #[serde(default)]
    pub remote_name: Option<String>,
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub worktree_path: Option<String>,
}

impl VcsRef {
    pub fn is_remote(&self) -> bool {
        self.is_remote == Some(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRefsResult {
    #[serde(default)]
    pub refs: Vec<VcsRef>,
    #[serde(default)]
    pub is_repo: bool,
    #[serde(default)]
    pub has_primary_remote: bool,
    #[serde(default)]
    pub next_cursor: Option<u64>,
    #[serde(default)]
    pub total_count: u64,
}

/// `VcsPullResult`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResult {
    /// `pulled` or `skipped_up_to_date`.
    pub status: String,
    pub ref_name: String,
    #[serde(default)]
    pub upstream_ref: Option<String>,
}

impl PullResult {
    /// The inline success the Git actions row shows after a pull.
    pub fn summary(&self) -> (String, String) {
        if self.status == "pulled" {
            ("Pulled".into(), format!("Updated {} from {}", self.ref_name, self.upstream_ref.as_deref().unwrap_or("upstream")))
        } else {
            ("Already up to date".into(), format!("{} is already synchronized.", self.ref_name))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefNameResult {
    #[serde(default)]
    pub ref_name: Option<String>,
}

pub fn status_subscription(cwd: &str) -> Value {
    json!({ "cwd": cwd })
}

pub fn refresh_status(cwd: &str, local_only: bool) -> Value {
    let mut payload = json!({ "cwd": cwd });
    if local_only {
        payload["localOnly"] = Value::Bool(true);
    }
    payload
}

pub fn pull(cwd: &str) -> Value {
    json!({ "cwd": cwd })
}

pub fn init(cwd: &str) -> Value {
    json!({ "cwd": cwd })
}

/// One page of `vcs.listRefs`. The query is the sanitized search, never blank.
pub fn list_refs(cwd: &str, query: Option<&str>, cursor: Option<u64>, limit: u64) -> Value {
    let mut payload = Map::new();
    payload.insert("cwd".into(), json!(cwd));
    if let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) {
        payload.insert("query".into(), json!(query.chars().take(256).collect::<String>()));
    }
    if let Some(cursor) = cursor {
        payload.insert("cursor".into(), json!(cursor));
    }
    payload.insert("limit".into(), json!(limit));
    Value::Object(payload)
}

pub fn switch_ref(cwd: &str, ref_name: &str) -> Value {
    json!({ "cwd": cwd, "refName": ref_name })
}

pub fn create_ref(cwd: &str, ref_name: &str) -> Value {
    json!({ "cwd": cwd, "refName": ref_name, "switchRef": true })
}

pub fn open_in_editor(cwd: &str, editor: &str) -> Value {
    json!({ "cwd": cwd, "editor": editor })
}

/// `EDITORS` (`packages/contracts/src/editor.ts`): ids in the web's preference order.
pub const EDITORS: &[(&str, &str)] = &[
    ("cursor", "Cursor"),
    ("trae", "Trae"),
    ("kiro", "Kiro"),
    ("vscode", "VS Code"),
    ("vscode-insiders", "VS Code Insiders"),
    ("vscodium", "VSCodium"),
    ("zed", "Zed"),
    ("antigravity", "Antigravity"),
    ("idea", "IntelliJ IDEA"),
    ("aqua", "Aqua"),
    ("clion", "CLion"),
    ("datagrip", "DataGrip"),
    ("dataspell", "DataSpell"),
    ("goland", "GoLand"),
    ("phpstorm", "PhpStorm"),
    ("pycharm", "PyCharm"),
    ("rider", "Rider"),
    ("rubymine", "RubyMine"),
    ("rustrover", "RustRover"),
    ("webstorm", "WebStorm"),
    ("file-manager", "File Manager"),
];

/// `editorLabelForPlatform`: the file manager takes the platform's name.
pub fn editor_label(id: &str, macos: bool) -> &'static str {
    match id {
        "file-manager" if macos => "Finder",
        "file-manager" => "Files",
        _ => EDITORS.iter().find(|(editor, _)| *editor == id).map_or("Editor", |(_, label)| label),
    }
}

/// The editors the picker lists: the server's available ones the client knows, in `EDITORS` order.
pub fn editor_options(available: &[String]) -> Vec<&'static str> {
    EDITORS.iter().map(|(id, _)| *id).filter(|id| available.iter().any(|editor| editor == id)).collect()
}

/// `usePreferredEditor`: the last editor used while it is still available, else the first one.
pub fn preferred_editor(last: Option<&str>, available: &[String]) -> Option<&'static str> {
    let options = editor_options(available);
    last.and_then(|last| options.iter().copied().find(|id| *id == last)).or_else(|| options.first().copied())
}

/// `thread.metadata.update` carrying the branch the thread now follows (`setThreadBranch`).
pub fn update_thread_branch(command_id: &str, thread_id: &str, branch: Option<&str>, worktree_path: Option<&str>) -> Value {
    json!({ "type": "thread.metadata.update", "commandId": command_id, "threadId": thread_id, "branch": branch, "worktreePath": worktree_path })
}

/// The feature branch a Git action created, guarded by the branch the thread had
/// (`resolveThreadBranchMetadataPatch`).
pub fn update_thread_branch_metadata(command_id: &str, thread_id: &str, branch: Option<&str>, expected_branch: Option<&str>) -> Value {
    json!({ "type": "thread.metadata.update", "commandId": command_id, "threadId": thread_id, "branch": branch, "expectedBranch": expected_branch })
}

/// `canUpdateThreadBranch` for a server thread: recording its branch needs `orchestration:operate`.
pub fn can_operate_thread(grants: Option<&AuthSessionState>) -> bool {
    grants.is_some_and(|grants| grants.grants(ORCHESTRATION_OPERATE_SCOPE))
}

/// `canChangeThreadBranch`: switching or creating a ref also writes to source control.
pub fn can_change_thread_branch(grants: Option<&AuthSessionState>) -> bool {
    can_operate_thread(grants) && grants.is_some_and(|grants| grants.grants(SOURCE_CONTROL_WRITE_SCOPE))
}

/// `sanitizeNewRefName`: refs cannot hold whitespace, so runs of it become one dash.
pub fn sanitize_new_ref_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for ch in raw.trim().chars() {
        if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{0c}' | '\u{0b}') {
            if !in_space {
                out.push('-');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

/// `deriveLocalBranchNameFromRemoteRef`: `origin/feature` checks out as `feature`.
pub fn local_branch_name_from_remote_ref(name: &str) -> &str {
    match name.find('/') {
        Some(index) if index > 0 && index < name.len() - 1 => &name[index + 1..],
        _ => name,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSelectionTarget {
    pub checkout_cwd: String,
    pub next_worktree_path: Option<String>,
    pub reuse_existing_worktree: bool,
}

/// `resolveBranchSelectionTarget`: a ref checked out in another worktree moves the thread there;
/// the default ref leaves a worktree for the project root; anything else checks out in place.
pub fn branch_selection_target(project_cwd: &str, worktree_path: Option<&str>, target: &VcsRef) -> BranchSelectionTarget {
    if let Some(path) = target.worktree_path.as_deref() {
        return BranchSelectionTarget {
            checkout_cwd: path.to_owned(),
            next_worktree_path: (path != project_cwd).then(|| path.to_owned()),
            reuse_existing_worktree: true,
        };
    }
    let next_worktree_path = if worktree_path.is_some() && target.is_default { None } else { worktree_path.map(str::to_owned) };
    BranchSelectionTarget {
        checkout_cwd: next_worktree_path.clone().unwrap_or_else(|| project_cwd.to_owned()),
        next_worktree_path,
        reuse_existing_worktree: false,
    }
}

/// `resolveBranchToolbarValue`. `selecting_worktree_base` is worktree mode with no worktree yet.
pub fn branch_toolbar_value(selecting_worktree_base: bool, thread_branch: Option<&str>, current_git_branch: Option<&str>) -> Option<String> {
    let value = if selecting_worktree_base { thread_branch.or(current_git_branch) } else { current_git_branch.or(thread_branch) };
    value.map(str::to_owned)
}

/// `resolveBranchTriggerLabel`.
pub fn branch_trigger_label(selecting_worktree_base: bool, branch: Option<&str>, branch_is_remote: Option<bool>, start_from_origin: bool) -> String {
    match branch {
        None => "Select ref".into(),
        Some(branch) if selecting_worktree_base && start_from_origin && branch_is_remote == Some(false) => format!("From {branch} (remote)"),
        Some(branch) if selecting_worktree_base => format!("From {branch}"),
        Some(branch) => branch.to_owned(),
    }
}

/// `resolveBranchToolbarPrBranch`: the PR row follows the thread's branch only while it is the
/// checked-out one.
pub fn pr_branch<'a>(thread_branch: Option<&'a str>, resolved_branch: Option<&str>) -> Option<&'a str> {
    thread_branch.filter(|branch| Some(*branch) == resolved_branch)
}

/// `shouldIncludeBranchPickerItem` for a ref name; the create and checkout rows always show.
pub fn ref_matches_query(name: &str, normalized_query: &str) -> bool {
    if normalized_query.is_empty() {
        return true;
    }
    let lower = name.to_lowercase();
    if lower.contains(normalized_query) {
        return true;
    }
    let sanitized = sanitize_new_ref_name(normalized_query);
    !sanitized.is_empty() && sanitized != normalized_query && lower.contains(&sanitized)
}

/// The branch picker's matching refs, as indices into the listing. While the listing holds and
/// the query only grows, it narrows the last result instead of rescanning every ref: whatever
/// matches the longer query matched its prefix.
#[derive(Debug, Clone, Default)]
pub struct RefFilter {
    revision: Option<u64>,
    query: String,
    indices: Vec<usize>,
}

impl RefFilter {
    /// `revision` changes whenever the listing does; `normalized_query` is trimmed and lowercase.
    pub fn update(&mut self, refs: &[VcsRef], revision: u64, normalized_query: &str) {
        if self.revision == Some(revision) && self.query == normalized_query {
            return;
        }
        if self.revision == Some(revision) && normalized_query.starts_with(self.query.as_str()) {
            self.indices.retain(|&index| ref_matches_query(&refs[index].name, normalized_query));
        } else {
            self.indices = refs.iter().enumerate().filter(|(_, item)| ref_matches_query(&item.name, normalized_query)).map(|(index, _)| index).collect();
        }
        self.revision = Some(revision);
        normalized_query.clone_into(&mut self.query);
    }

    pub fn indices(&self) -> &[usize] {
        &self.indices
    }
}

/// `BranchPickerRefItem`'s trailing badge.
pub fn ref_badge(target: &VcsRef, project_cwd: Option<&str>) -> Option<&'static str> {
    let secondary_worktree = matches!((target.worktree_path.as_deref(), project_cwd), (Some(path), Some(root)) if path != root);
    if target.current {
        Some("current")
    } else if secondary_worktree {
        Some("worktree")
    } else if target.is_remote() {
        Some("remote")
    } else if target.is_default {
        Some("default")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
