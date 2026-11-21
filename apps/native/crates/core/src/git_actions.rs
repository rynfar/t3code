//! Git quick actions: what the split button offers, why an action is unavailable, the default
//! branch confirmation, the Changes row's totals and base, and the live progress of a running
//! action. Ports `apps/web/src/components/GitActionsControl.logic.ts` (including
//! `resolveChangesTotals`), the disabled reasons and inline success in
//! `GitActionsControl.tsx`, `requiresDefaultBranchConfirmation`
//! (`client-runtime/src/state/gitActions.ts`), the progress reducer in
//! `client-runtime/src/state/vcsAction.ts`, and each source control definition's change request
//! terms (`packages/source-control-*/src/shared/definition.ts`).

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::diff::DiffPreviewSource;
use crate::vcs::{VcsStatus, changes_totals};

pub mod method {
    pub const SOURCE_CONTROL_DISCOVERY: &str = "server.discoverSourceControl";
    pub const SOURCE_CONTROL_PUBLISH_REPOSITORY: &str = "sourceControl.publishRepository";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackedAction {
    Commit,
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
}

impl StackedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            StackedAction::Commit => "commit",
            StackedAction::Push => "push",
            StackedAction::CreatePr => "create_pr",
            StackedAction::CommitPush => "commit_push",
            StackedAction::CommitPushPr => "commit_push_pr",
        }
    }

    fn can_commit(self) -> bool {
        matches!(self, StackedAction::Commit | StackedAction::CommitPush | StackedAction::CommitPushPr)
    }
}

/// `ChangeRequestTerminology`: GitLab says merge request, the rest pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminology {
    pub short_label: &'static str,
    pub singular: &'static str,
}

/// `sourceControlClients.get(kind).changeRequest`: no provider means the first definition
/// (GitHub); a kind no definition claims is a generic change request.
pub fn terminology(kind: Option<&str>) -> Terminology {
    match kind {
        None | Some("github" | "gitcafe" | "forgejo" | "bitbucket" | "azure-devops") => Terminology { short_label: "PR", singular: "pull request" },
        Some("gitlab") => Terminology { short_label: "MR", singular: "merge request" },
        Some(_) => Terminology { short_label: "change request", singular: "change request" },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItemId {
    Commit,
    Push,
    Pr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub id: MenuItemId,
    pub label: String,
    pub disabled: bool,
}

fn has_default_branch_delta(status: &VcsStatus) -> bool {
    status.remote.ahead_of_default_count.unwrap_or(status.remote.ahead_count) > 0
}

/// `buildMenuItems`: Commit, then Push and Create PR when there is a primary remote; an open PR
/// drops Create PR.
pub fn build_menu_items(status: Option<&VcsStatus>, busy: bool, has_primary_remote: bool) -> Vec<MenuItem> {
    let Some(status) = status else { return Vec::new() };
    let terms = terminology(status.provider_kind());
    let has_branch = status.local.ref_name.is_some();
    let behind = status.remote.behind_count > 0;
    let can_push_without_upstream = has_primary_remote && !status.remote.has_upstream;
    let reachable = status.remote.has_upstream || can_push_without_upstream;
    let commit = MenuItem { id: MenuItemId::Commit, label: "Commit".into(), disabled: busy || !status.local.has_working_tree_changes };
    if !has_primary_remote {
        return vec![commit];
    }
    let push = MenuItem { id: MenuItemId::Push, label: "Push".into(), disabled: !(!busy && has_branch && !behind && status.remote.ahead_count > 0 && reachable) };
    if status.has_open_pr() {
        return vec![commit, push];
    }
    let can_create_pr = !busy && has_branch && has_default_branch_delta(status) && !behind && reachable;
    vec![commit, push, MenuItem { id: MenuItemId::Pr, label: format!("Create {}", terms.short_label), disabled: !can_create_pr }]
}

/// `hasUsableGitMenuContent`: the chevron menu opens only when it can publish, shows a notice,
/// or holds an enabled action this connection can run.
pub fn has_usable_menu_content(items: &[MenuItem], can_write_source_control: bool, can_publish_repository: bool, has_notice: bool) -> bool {
    can_publish_repository || has_notice || (can_write_source_control && items.iter().any(|item| !item.disabled))
}

/// The default source control host (`sourceControlClients.get(undefined).kind`).
pub const DEFAULT_SOURCE_CONTROL_PROVIDER: &str = "github";

/// `isSourceControlProviderOffered`: only the default host is offered until the user turns
/// others on (`ServerSettings.offeredSourceControlProviders`).
pub fn is_source_control_provider_offered(offered: Option<&HashMap<String, bool>>, kind: &str) -> bool {
    offered.and_then(|map| map.get(kind).copied()).unwrap_or(kind == DEFAULT_SOURCE_CONTROL_PROVIDER)
}

/// Same rule as [`is_source_control_provider_offered`] over `ServerSettings.source_control.offered_source_control_providers`.
pub fn is_provider_offered(offered: Option<&BTreeMap<String, bool>>, kind: &str) -> bool {
    offered.and_then(|map| map.get(kind).copied()).unwrap_or(kind == DEFAULT_SOURCE_CONTROL_PROVIDER)
}

/// `getMenuActionDisabledReason` in `GitActionsControl.tsx`.
pub fn menu_disabled_reason(item: &MenuItem, status: Option<&VcsStatus>, busy: bool, has_primary_remote: bool) -> Option<String> {
    if !item.disabled {
        return None;
    }
    if busy {
        return Some("Git action in progress.".into());
    }
    let Some(status) = status else { return Some("Git status is unavailable.".into()) };
    let has_branch = status.local.ref_name.is_some();
    let has_changes = status.local.has_working_tree_changes;
    let behind = status.remote.behind_count > 0;
    let terms = terminology(status.provider_kind());
    let reason = match item.id {
        MenuItemId::Commit if !has_changes => "Worktree is clean. Make changes before committing.".to_owned(),
        MenuItemId::Commit => "Commit is currently unavailable.".to_owned(),
        MenuItemId::Push if !has_branch => "Detached HEAD: check out a branch before pushing.".to_owned(),
        MenuItemId::Push if has_changes => "Commit or stash local changes before pushing.".to_owned(),
        MenuItemId::Push if behind => "Branch is behind upstream. Pull/rebase before pushing.".to_owned(),
        MenuItemId::Push if !status.remote.has_upstream && !has_primary_remote => "Add an \"origin\" remote before pushing.".to_owned(),
        MenuItemId::Push if status.remote.ahead_count == 0 => "No local commits to push.".to_owned(),
        MenuItemId::Push => "Push is currently unavailable.".to_owned(),
        MenuItemId::Pr if !has_branch => format!("Detached HEAD: check out a branch before creating a {}.", terms.singular),
        MenuItemId::Pr if !status.remote.has_upstream && !has_primary_remote => format!("Add an \"origin\" remote before creating a {}.", terms.singular),
        MenuItemId::Pr if !has_default_branch_delta(status) => format!("No local commits to include in a {}.", terms.singular),
        MenuItemId::Pr if behind => format!("Branch is behind upstream. Pull/rebase before creating a {}.", terms.singular),
        MenuItemId::Pr => format!("Create {} is currently unavailable.", terms.singular),
    };
    Some(reason)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickActionKind {
    Run(StackedAction),
    Pull,
    Publish,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickActionIcon {
    Commit,
    Push,
    Pull,
    ChangeRequest,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickAction {
    pub label: String,
    pub disabled: bool,
    pub kind: QuickActionKind,
    pub hint: Option<String>,
}

impl QuickAction {
    fn run(label: impl Into<String>, action: StackedAction) -> Self {
        QuickAction { label: label.into(), disabled: false, kind: QuickActionKind::Run(action), hint: None }
    }

    fn hint(label: &str, hint: impl Into<String>) -> Self {
        QuickAction { label: label.into(), disabled: true, kind: QuickActionKind::Hint, hint: Some(hint.into()) }
    }

    /// `GitQuickActionIcon`.
    pub fn icon(&self) -> QuickActionIcon {
        match self.kind {
            QuickActionKind::Publish => QuickActionIcon::Push,
            QuickActionKind::Pull => QuickActionIcon::Pull,
            QuickActionKind::Run(StackedAction::Commit) => QuickActionIcon::Commit,
            QuickActionKind::Run(StackedAction::Push | StackedAction::CommitPush) => QuickActionIcon::Push,
            QuickActionKind::Run(_) => QuickActionIcon::ChangeRequest,
            QuickActionKind::Hint if self.label == "Commit" => QuickActionIcon::Commit,
            QuickActionKind::Hint if self.label == "Push" => QuickActionIcon::Push,
            QuickActionKind::Hint => QuickActionIcon::Info,
        }
    }

    /// `quickActionDisabledReason`, given the connection can write source control.
    pub fn disabled_reason(&self) -> Option<String> {
        self.disabled.then(|| self.hint.clone().unwrap_or_else(|| "This action is currently unavailable.".into()))
    }
}

/// `resolveQuickAction`: the one action the primary button runs for this status.
pub fn resolve_quick_action(status: Option<&VcsStatus>, busy: bool, is_default_ref: bool, has_primary_remote: bool) -> QuickAction {
    const UP_TO_DATE: &str = "Branch is up to date. No action needed.";
    if busy {
        return QuickAction::hint("Commit", "Git action in progress.");
    }
    let Some(status) = status else { return QuickAction::hint("Commit", "Git status is unavailable.") };
    let terms = terminology(status.provider_kind());
    let has_open_pr = status.has_open_pr();
    let ahead = status.remote.ahead_count > 0;
    let behind = status.remote.behind_count > 0;
    if status.local.ref_name.is_none() {
        return QuickAction::hint("Commit", format!("Create and checkout a ref before pushing or opening a {}.", terms.singular));
    }
    if status.local.has_working_tree_changes {
        if behind || (!status.remote.has_upstream && !has_primary_remote) {
            return QuickAction::run("Commit", StackedAction::Commit);
        }
        if has_open_pr || is_default_ref {
            return QuickAction::run("Commit & push", StackedAction::CommitPush);
        }
        return QuickAction::run(format!("Commit, push & {}", terms.short_label), StackedAction::CommitPushPr);
    }
    let push = if is_default_ref { StackedAction::CommitPush } else { StackedAction::Push };
    if !status.remote.has_upstream {
        if !has_primary_remote {
            return QuickAction { label: "Publish repository".into(), disabled: false, kind: QuickActionKind::Publish, hint: None };
        }
        if !ahead {
            return if has_open_pr { QuickAction::hint("Commit", UP_TO_DATE) } else { QuickAction::hint("Push", "No local commits to push.") };
        }
        if has_open_pr || is_default_ref {
            return QuickAction::run("Push", push);
        }
        return QuickAction::run(format!("Push & create {}", terms.short_label), StackedAction::CreatePr);
    }
    if ahead && behind {
        return QuickAction::hint("Sync ref", "Branch has diverged from upstream. Rebase/merge first.");
    }
    if behind {
        return QuickAction { label: "Pull".into(), disabled: false, kind: QuickActionKind::Pull, hint: None };
    }
    if ahead {
        if has_open_pr || is_default_ref {
            return QuickAction::run("Push", push);
        }
        return QuickAction::run(format!("Push & create {}", terms.short_label), StackedAction::CreatePr);
    }
    if has_open_pr {
        return QuickAction::hint("Commit", UP_TO_DATE);
    }
    if has_default_branch_delta(status) && !is_default_ref {
        return QuickAction::run(format!("Create {}", terms.short_label), StackedAction::CreatePr);
    }
    QuickAction::hint("Commit", UP_TO_DATE)
}

/// `requiresDefaultBranchConfirmation`: anything that pushes from the default branch asks first.
pub fn requires_default_branch_confirmation(action: StackedAction, is_default_branch: bool) -> bool {
    is_default_branch && action != StackedAction::Commit
}

/// Whether the action will create a commit (`runGitActionWithToast`'s `includesCommit`).
pub fn includes_commit(action: StackedAction, has_working_tree_changes: bool, feature_branch: bool) -> bool {
    action.can_commit() && (action == StackedAction::Commit || has_working_tree_changes || feature_branch)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogCopy {
    pub title: String,
    pub description: String,
    pub continue_label: String,
}

/// `resolveDefaultBranchActionDialogCopy`.
pub fn default_branch_dialog_copy(action: StackedAction, branch: &str, includes_commit: bool, terms: Terminology) -> DialogCopy {
    let suffix = format!(" on \"{branch}\". You can continue on this ref or create a feature ref and run the same action there.");
    let short = terms.short_label;
    let (title, description, continue_label) = match (matches!(action, StackedAction::Push | StackedAction::CommitPush), includes_commit) {
        (true, true) => ("Commit & push to default ref?".to_owned(), format!("This action will commit and push changes{suffix}"), format!("Commit & push to {branch}")),
        (true, false) => ("Push to default ref?".to_owned(), format!("This action will push local commits{suffix}"), format!("Push to {branch}")),
        (false, true) => (
            format!("Commit, push & create {short} from default ref?"),
            format!("This action will commit, push, and create a {}{suffix}", terms.singular),
            format!("Commit, push & create {short}"),
        ),
        (false, false) => (
            format!("Push & create {short} from default ref?"),
            format!("This action will push local commits and create a {}{suffix}", terms.singular),
            format!("Push & create {short}"),
        ),
    };
    DialogCopy { title, description, continue_label }
}

/// `resolveFeatureBranchUnavailableReason`: the server names feature branches from the changes.
pub fn feature_branch_unavailable_reason(branch: &str, has_working_tree_changes: bool) -> Option<String> {
    (!has_working_tree_changes)
        .then(|| format!("There are no changes to commit, so a feature branch can't be named. Create a branch yourself or continue on {branch}."))
}

/// `quietEmptyDraft`: a thread with no runs yet hides the commit, push and diff rows until there
/// is work to act on.
pub fn quiet_empty_thread(no_runs_yet: bool, running: bool, status_error: bool, quick: &QuickAction, status: Option<&VcsStatus>) -> bool {
    let (insertions, deletions) = status.map(changes_totals).unwrap_or((0, 0));
    no_runs_yet && !running && !status_error && quick.disabled && insertions == 0 && deletions == 0
}

/// `ChangesLineTotals`: what the Changes row counts and the ref it compares against; `None` means
/// uncommitted changes against HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesTotals {
    pub insertions: u64,
    pub deletions: u64,
    pub base_ref: Option<String>,
}

impl ChangesTotals {
    /// `changesBaseLabel`: the muted label beside "Changes".
    pub fn base_label(&self) -> String {
        match &self.base_ref {
            Some(base_ref) => format!("vs {base_ref}"),
            None => "Uncommitted".to_owned(),
        }
    }
}

/// `resolveChangesTotals`: the diff panel's branch comparison when it has file stats, else the
/// branch totals unless they are zero and the working tree is not.
pub fn resolve_changes_totals(status: Option<&VcsStatus>, branch_preview: Option<&DiffPreviewSource>) -> Option<ChangesTotals> {
    if let Some(preview) = branch_preview
        && let Some((insertions, deletions)) = preview.totals
    {
        return Some(ChangesTotals { insertions, deletions, base_ref: preview.base_ref.clone() });
    }
    let local = &status?.local;
    let tree = &local.working_tree;
    let tree_totals = || ChangesTotals { insertions: tree.insertions, deletions: tree.deletions, base_ref: None };
    let branch_totals = |branch: &crate::vcs::BranchChanges| ChangesTotals { insertions: branch.insertions, deletions: branch.deletions, base_ref: branch.base_ref.clone() };
    Some(match &local.branch_changes {
        Some(branch) if branch.insertions > 0 || branch.deletions > 0 => branch_totals(branch),
        _ if tree.insertions > 0 || tree.deletions > 0 => tree_totals(),
        Some(branch) => branch_totals(branch),
        None => tree_totals(),
    })
}

#[derive(Debug, Clone, Default)]
pub struct RunRequest<'a> {
    pub commit_message: Option<&'a str>,
    pub feature_branch: bool,
    pub file_paths: Option<&'a [String]>,
    pub thread_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
}

/// `GitRunStackedActionInput`.
pub fn run_stacked_action(action_id: &str, cwd: &str, action: StackedAction, request: &RunRequest<'_>) -> Value {
    let mut payload = Map::new();
    payload.insert("actionId".into(), json!(action_id));
    payload.insert("cwd".into(), json!(cwd));
    payload.insert("action".into(), json!(action.as_str()));
    if let Some(message) = request.commit_message.map(str::trim).filter(|message| !message.is_empty()) {
        payload.insert("commitMessage".into(), json!(message.chars().take(10_000).collect::<String>()));
    }
    if request.feature_branch {
        payload.insert("featureBranch".into(), json!(true));
    }
    if let Some(paths) = request.file_paths.filter(|paths| !paths.is_empty()) {
        payload.insert("filePaths".into(), json!(paths));
    }
    if let Some(thread_id) = request.thread_id {
        payload.insert("threadId".into(), json!(thread_id));
    }
    if let Some(project_id) = request.project_id {
        payload.insert("projectId".into(), json!(project_id));
    }
    Value::Object(payload)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StepStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ResultToast {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// `GitRunStackedActionResult`, the parts the client reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RunResult {
    #[serde(default)]
    pub branch: StepStatus,
    #[serde(default)]
    pub pr: StepStatus,
    #[serde(default)]
    pub toast: ResultToast,
}

impl RunResult {
    /// The inline success: a new PR reads "Created PR"; anything else shows the server's toast.
    pub fn summary(&self, terms: Terminology) -> (String, Option<String>) {
        if self.pr.status == "created" {
            (format!("Created {}", terms.short_label), None)
        } else {
            (self.toast.title.clone(), self.toast.description.clone())
        }
    }

    /// `resolveThreadBranchUpdate`: the feature branch the action created, if it made one.
    pub fn created_branch(&self) -> Option<&str> {
        (self.branch.status == "created").then_some(self.branch.name.as_deref()).flatten()
    }
}

/// `GitActionProgressEvent`, one chunk of `git.runStackedAction`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ProgressEvent {
    ActionStarted { action_id: String },
    PhaseStarted { action_id: String, label: String },
    HookStarted { action_id: String, hook_name: String },
    HookOutput { action_id: String, text: String },
    HookFinished { action_id: String },
    ActionFinished { action_id: String, result: RunResult },
    ActionFailed { action_id: String, #[serde(default)] phase: Option<String> },
    #[serde(other)]
    Unknown,
}

impl ProgressEvent {
    fn action_id(&self) -> Option<&str> {
        match self {
            ProgressEvent::ActionStarted { action_id }
            | ProgressEvent::PhaseStarted { action_id, .. }
            | ProgressEvent::HookStarted { action_id, .. }
            | ProgressEvent::HookOutput { action_id, .. }
            | ProgressEvent::HookFinished { action_id }
            | ProgressEvent::ActionFinished { action_id, .. }
            | ProgressEvent::ActionFailed { action_id, .. } => Some(action_id),
            ProgressEvent::Unknown => None,
        }
    }
}

/// `VcsActionRemoteFailureError`'s message. The server's own text can echo credentials from
/// git's stderr, so only the phase reaches the user.
pub fn remote_failure_message(action: StackedAction, phase: Option<&str>) -> String {
    format!("Source control action '{}' failed during {}.", action.as_str(), phase.unwrap_or("execution"))
}

/// `VcsActionMissingTerminalEventError`'s message.
pub fn missing_terminal_message(action: StackedAction) -> String {
    format!("Source control action '{}' ended without a terminal result.", action.as_str())
}

/// One `git.runStackedAction` stream, consumed to its end (`consumeVcsActionProgress`). After
/// `action_finished` the server still links the PR and refreshes status before it ends the
/// stream, so the result is only read once the stream is over.
#[derive(Debug, Clone)]
pub struct StackedActionRun {
    action_id: String,
    action: StackedAction,
    terminal: Option<ProgressEvent>,
}

impl StackedActionRun {
    pub fn new(action_id: &str, action: StackedAction) -> Self {
        StackedActionRun { action_id: action_id.to_owned(), action, terminal: None }
    }

    /// Notes this action's terminal event; the stream stays open until the server ends it.
    pub fn observe(&mut self, event: &ProgressEvent) {
        if matches!(event, ProgressEvent::ActionFinished { .. } | ProgressEvent::ActionFailed { .. }) && event.action_id() == Some(self.action_id.as_str()) {
            self.terminal = Some(event.clone());
        }
    }

    /// The outcome once the stream ended, or failed with `stream_error`.
    pub fn settle(self, stream_error: Option<String>) -> Result<RunResult, String> {
        match (self.terminal, stream_error) {
            (Some(ProgressEvent::ActionFailed { phase, .. }), _) => Err(remote_failure_message(self.action, phase.as_deref())),
            (_, Some(error)) => Err(error),
            (Some(ProgressEvent::ActionFinished { result, .. }), None) => Ok(result),
            _ => Err(missing_terminal_message(self.action)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    RunChangeRequest,
    Pull,
}

/// `VcsActionState` while an action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionProgress {
    pub operation: Operation,
    pub action_id: String,
    pub current_label: String,
    current_phase_label: String,
    pub last_output_line: Option<String>,
    pub phase_started_at_ms: i64,
    pub hook_started_at_ms: Option<i64>,
}

/// What the running row shows (`resolveGitActionProgressPresentation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressPresentation {
    pub status: String,
    pub output: Option<String>,
    pub started_at_ms: i64,
}

impl ActionProgress {
    /// `beginVcsActionState`.
    pub fn begin(operation: Operation, action_id: &str, now_ms: i64) -> Self {
        let label = match operation {
            Operation::RunChangeRequest => "Running source control action",
            Operation::Pull => "Pulling latest changes...",
        };
        ActionProgress {
            operation,
            action_id: action_id.to_owned(),
            current_label: label.into(),
            current_phase_label: label.into(),
            last_output_line: None,
            phase_started_at_ms: now_ms,
            hook_started_at_ms: None,
        }
    }

    /// `applyVcsActionProgressEvent`. Events for another action are ignored.
    pub fn apply(&mut self, event: &ProgressEvent, now_ms: i64) {
        if event.action_id() != Some(self.action_id.as_str()) {
            return;
        }
        match event {
            ProgressEvent::ActionStarted { .. } => {
                self.phase_started_at_ms = now_ms;
                self.hook_started_at_ms = None;
                self.last_output_line = None;
            }
            ProgressEvent::PhaseStarted { label, .. } => {
                self.current_label = label.clone();
                self.current_phase_label = label.clone();
                self.phase_started_at_ms = now_ms;
                self.hook_started_at_ms = None;
                self.last_output_line = None;
            }
            ProgressEvent::HookStarted { hook_name, .. } => {
                self.current_label = format!("Running {hook_name}...");
                self.hook_started_at_ms = Some(now_ms);
                self.last_output_line = None;
            }
            ProgressEvent::HookOutput { text, .. } => self.last_output_line = Some(text.clone()),
            ProgressEvent::HookFinished { .. } => {
                self.current_label = self.current_phase_label.clone();
                self.hook_started_at_ms = None;
                self.last_output_line = None;
            }
            ProgressEvent::ActionFinished { .. } | ProgressEvent::ActionFailed { .. } | ProgressEvent::Unknown => {}
        }
    }

    pub fn presentation(&self) -> ProgressPresentation {
        let label = self.current_label.trim();
        let pull = self.operation == Operation::Pull;
        let status = if !label.is_empty() && label != "Running source control action" {
            label.to_owned()
        } else if pull {
            "Pulling latest changes...".to_owned()
        } else {
            "Starting source control action...".to_owned()
        };
        let output = self.last_output_line.as_deref().map(str::trim).filter(|line| !pull && !line.is_empty()).map(str::to_owned);
        let started_at_ms = if pull { self.phase_started_at_ms } else { self.hook_started_at_ms.unwrap_or(self.phase_started_at_ms) };
        ProgressPresentation { status, output, started_at_ms }
    }
}

/// `formatGitActionElapsed`.
pub fn format_elapsed(started_at_ms: i64, now_ms: i64) -> String {
    let seconds = ((now_ms - started_at_ms) / 1_000).max(0);
    if seconds < 60 { format!("{seconds}s") } else { format!("{}m {}s", seconds / 60, seconds % 60) }
}

/// `GIT_ACTION_SUCCESS_VISIBLE_MS`: how long the inline success replaces the button.
pub const SUCCESS_VISIBLE_MS: u64 = 10_000;

pub const COMMIT_DIALOG_TITLE: &str = "Commit changes";
pub const COMMIT_DIALOG_DESCRIPTION: &str = "Review and confirm your commit. Leave the message blank to auto-generate one.";

/// Static metadata for a publishable source control provider (`PUBLISH_PROVIDER_OPTIONS` in
/// `GitActionsControl.tsx`, derived from `packages/source-control-*/src/shared/definition.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishProviderOption {
    pub kind: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub path_placeholder: &'static str,
    /// `None` uses the signed-in host (falling back to `"your server"`), as Forgejo does.
    pub fixed_host: Option<&'static str>,
}

impl PublishProviderOption {
    pub fn publish_host(&self, signed_in_host: Option<&str>) -> String {
        match self.fixed_host {
            Some(host) => host.to_owned(),
            None => signed_in_host.map(str::trim).filter(|h| !h.is_empty()).unwrap_or("your server").to_owned(),
        }
    }
}

pub const PUBLISH_PROVIDER_OPTIONS: &[PublishProviderOption] = &[
    PublishProviderOption {
        kind: "github",
        label: "GitHub",
        description: "github.com",
        path_placeholder: "owner/repo",
        fixed_host: Some("github.com"),
    },
    PublishProviderOption {
        kind: "gitlab",
        label: "GitLab",
        description: "gitlab.com",
        path_placeholder: "group/project",
        fixed_host: Some("gitlab.com"),
    },
    PublishProviderOption {
        kind: "azure-devops",
        label: "Azure DevOps",
        description: "dev.azure.com",
        path_placeholder: "project/repository",
        fixed_host: Some("dev.azure.com"),
    },
    PublishProviderOption {
        kind: "bitbucket",
        label: "Bitbucket",
        description: "bitbucket.org",
        path_placeholder: "workspace/repository",
        fixed_host: Some("bitbucket.org"),
    },
    PublishProviderOption {
        kind: "forgejo",
        label: "Forgejo / Gitea",
        description: "Your signed-in server",
        path_placeholder: "owner/repo",
        fixed_host: None,
    },
    PublishProviderOption {
        kind: "gitcafe",
        label: "GitCafe",
        description: "git.cafe",
        path_placeholder: "owner/repo",
        fixed_host: Some("git.cafe"),
    },
    PublishProviderOption {
        kind: "cursor-origin",
        label: "Cursor Origin",
        description: "origin.cursor.com",
        path_placeholder: "owner/repo",
        fixed_host: Some("origin.cursor.com"),
    },
];

pub fn publish_provider_option(kind: &str) -> &'static PublishProviderOption {
    PUBLISH_PROVIDER_OPTIONS.iter().find(|option| option.kind == kind).unwrap_or(&PUBLISH_PROVIDER_OPTIONS[0])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishProviderDiscovery {
    pub kind: String,
    pub label: String,
    pub available: bool,
    pub install_hint: String,
    pub auth_status: String,
    pub auth_account: Option<String>,
    pub auth_host: Option<String>,
    pub auth_detail: Option<String>,
}

fn decode_effect_option_str(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let inner = match value.get("_tag").and_then(Value::as_str) {
        Some("Some") => value.get("value")?,
        Some(_) => return None,
        None => value,
    };
    inner.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// Decodes `sourceControlProviders` from `server.discoverSourceControl`.
pub fn decode_publish_discovery(value: &Value) -> Vec<PublishProviderDiscovery> {
    let Some(items) = value.get("sourceControlProviders").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let kind = item.get("kind")?.as_str()?.trim();
            if kind.is_empty() {
                return None;
            }
            let auth = item.get("auth");
            Some(PublishProviderDiscovery {
                kind: kind.to_owned(),
                label: item.get("label").and_then(Value::as_str).unwrap_or(kind).to_owned(),
                available: item.get("status").and_then(Value::as_str) == Some("available"),
                install_hint: item.get("installHint").and_then(Value::as_str).unwrap_or_default().to_owned(),
                auth_status: auth.and_then(|a| a.get("status")).and_then(Value::as_str).unwrap_or("unknown").to_owned(),
                auth_account: decode_effect_option_str(auth.and_then(|a| a.get("account"))),
                auth_host: decode_effect_option_str(auth.and_then(|a| a.get("host"))),
                auth_detail: decode_effect_option_str(auth.and_then(|a| a.get("detail"))),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishProviderReadiness {
    pub ready: bool,
    pub hint: Option<String>,
}

/// `getPublishProviderReadiness` in `GitActionsControl.tsx`.
pub fn publish_provider_readiness(provider: &str, discovered: &[PublishProviderDiscovery]) -> PublishProviderReadiness {
    let Some(item) = discovered.iter().find(|entry| entry.kind == provider) else {
        return PublishProviderReadiness {
            ready: false,
            hint: Some("Provider status unavailable. Open Settings -> Source control and rescan.".into()),
        };
    };
    if !item.available {
        return PublishProviderReadiness { ready: false, hint: Some(item.install_hint.clone()).filter(|h| !h.is_empty()) };
    }
    if item.auth_status == "unauthenticated" {
        return PublishProviderReadiness {
            ready: false,
            hint: Some(
                item.auth_detail
                    .clone()
                    .unwrap_or_else(|| format!("{} is not authenticated. Open Settings -> Source control for setup guidance.", item.label)),
            ),
        };
    }
    PublishProviderReadiness { ready: true, hint: None }
}

/// Offered providers sorted ready-first, then alphabetically by label (`sortedPublishProviderOptions`).
pub fn offered_publish_providers(
    offered: Option<&BTreeMap<String, bool>>,
    discovered: &[PublishProviderDiscovery],
) -> Vec<(&'static PublishProviderOption, PublishProviderReadiness)> {
    let mut list: Vec<_> = PUBLISH_PROVIDER_OPTIONS
        .iter()
        .filter(|option| is_provider_offered(offered, option.kind))
        .map(|option| (option, publish_provider_readiness(option.kind, discovered)))
        .collect();
    list.sort_by(|(left, left_ready), (right, right_ready)| {
        right_ready.ready.cmp(&left_ready.ready).then_with(|| left.label.cmp(right.label))
    });
    list
}

/// `publishProvider` selection rule in `PublishRepositoryDialog`: an offered & ready user choice
/// wins, else the first ready offered provider, else the offered selection, else GitHub.
pub fn resolve_publish_provider(
    selected: Option<&str>,
    offered: Option<&BTreeMap<String, bool>>,
    discovered: &[PublishProviderDiscovery],
) -> &'static PublishProviderOption {
    let offered_list = offered_publish_providers(offered, discovered);
    let offered_selection = selected.filter(|kind| is_provider_offered(offered, kind));
    if let Some(kind) = offered_selection
        && publish_provider_readiness(kind, discovered).ready
    {
        return publish_provider_option(kind);
    }
    if let Some((first_ready, _)) = offered_list.iter().find(|(_, readiness)| readiness.ready) {
        return first_ready;
    }
    if let Some(kind) = offered_selection {
        return publish_provider_option(kind);
    }
    publish_provider_option(DEFAULT_SOURCE_CONTROL_PROVIDER)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PublishVisibility {
    #[default]
    Private,
    Public,
}

impl PublishVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            PublishVisibility::Private => "private",
            PublishVisibility::Public => "public",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CloneProtocol {
    #[default]
    Ssh,
    Https,
}

impl CloneProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            CloneProtocol::Ssh => "ssh",
            CloneProtocol::Https => "https",
        }
    }
}

/// Validates `<owner>/<repo>` (`publishRepositoryOwner` & `publishRepositoryName` in `GitActionsControl.tsx`).
pub fn valid_publish_repository(repository: &str) -> bool {
    let trimmed = repository.trim();
    let Some((owner, name)) = trimmed.split_once('/') else {
        return false;
    };
    !owner.trim().is_empty() && !name.trim().is_empty()
}

/// `SourceControlPublishRepositoryInput`.
pub fn publish_repository_input(
    cwd: &str,
    provider: &str,
    repository: &str,
    visibility: PublishVisibility,
    remote_name: &str,
    protocol: CloneProtocol,
) -> Value {
    let remote = remote_name.trim();
    json!({
        "cwd": cwd,
        "provider": provider,
        "repository": repository.trim(),
        "visibility": visibility.as_str(),
        "remoteName": if remote.is_empty() { "origin" } else { remote },
        "protocol": protocol.as_str(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedRepositoryInfo {
    pub name_with_owner: String,
    pub url: String,
}

/// `SourceControlPublishRepositoryResult`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRepositoryResult {
    pub repository: PublishedRepositoryInfo,
    pub remote_name: String,
    #[serde(default)]
    pub remote_url: String,
    pub branch: String,
    /// `"pushed"` or `"remote_added"`.
    pub status: String,
}

/// Resolves a changed file from the commit dialog against `workingTree.rootPath`
/// (`openChangedFileInEditor` → `resolvePathLinkTarget`).
pub fn resolve_commit_dialog_file_path(file_path: &str, working_tree_root: &str) -> Option<String> {
    let file = file_path.trim();
    let root = working_tree_root.trim();
    if file.is_empty() || root.is_empty() {
        return None;
    }
    if file.starts_with('/') || file.starts_with("\\\\") || (file.len() >= 3 && file.as_bytes()[1] == b':' && matches!(file.as_bytes()[2], b'/' | b'\\')) {
        return Some(file.to_owned());
    }
    let sep = if root.contains('\\') && !root.contains('/') { '\\' } else { '/' };
    let trimmed_root = root.trim_end_matches(['/', '\\']);
    let trimmed_file = file.trim_start_matches(['/', '\\']);
    if trimmed_file.is_empty() {
        return None;
    }
    Some(format!("{trimmed_root}{sep}{trimmed_file}"))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod publish_tests {
    use super::*;

    fn discovered(kind: &str, available: bool, auth_status: &str, account: Option<&str>, host: Option<&str>) -> PublishProviderDiscovery {
        PublishProviderDiscovery {
            kind: kind.into(),
            label: publish_provider_option(kind).label.into(),
            available,
            install_hint: format!("Install {kind}"),
            auth_status: auth_status.into(),
            auth_account: account.map(str::to_owned),
            auth_host: host.map(str::to_owned),
            auth_detail: None,
        }
    }

    #[test]
    fn offered_providers_default_to_github_only_and_sort_ready_first() {
        let items = vec![
            discovered("github", true, "unauthenticated", None, None),
            discovered("gitlab", true, "authenticated", Some("alice"), None),
        ];
        let default_offered = offered_publish_providers(None, &items);
        assert_eq!(default_offered.len(), 1);
        assert_eq!(default_offered[0].0.kind, "github");
        assert!(!default_offered[0].1.ready);

        let map = BTreeMap::from([("github".to_owned(), true), ("gitlab".to_owned(), true)]);
        let both = offered_publish_providers(Some(&map), &items);
        assert_eq!(both.iter().map(|(o, _)| o.kind).collect::<Vec<_>>(), vec!["gitlab", "github"]);
        assert_eq!(resolve_publish_provider(None, Some(&map), &items).kind, "gitlab");
        assert_eq!(resolve_publish_provider(Some("gitlab"), Some(&BTreeMap::from([("gitlab".to_owned(), false)])), &items).kind, "github");
    }

    #[test]
    fn decode_publish_discovery_reads_effect_options_and_forgejo_host() {
        let payload = json!({
            "sourceControlProviders": [{
                "kind": "forgejo",
                "label": "Forgejo / Gitea",
                "status": "available",
                "installHint": "Install tea",
                "auth": {
                    "status": "authenticated",
                    "account": { "_tag": "Some", "value": "bob" },
                    "host": { "_tag": "Some", "value": "git.example.test" },
                    "detail": { "_tag": "None" }
                }
            }]
        });
        let items = decode_publish_discovery(&payload);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].auth_account.as_deref(), Some("bob"));
        assert_eq!(items[0].auth_host.as_deref(), Some("git.example.test"));
        assert_eq!(publish_provider_option("forgejo").publish_host(items[0].auth_host.as_deref()), "git.example.test");
        assert_eq!(publish_provider_option("forgejo").publish_host(None), "your server");
    }

    #[test]
    fn valid_publish_repository_and_commit_dialog_file_path() {
        assert!(valid_publish_repository("alice/demo"));
        assert!(valid_publish_repository("group/sub/project"));
        assert!(!valid_publish_repository("alice/"));
        assert!(!valid_publish_repository("/demo"));
        assert!(!valid_publish_repository("nodivider"));
        assert_eq!(resolve_commit_dialog_file_path("src/main.rs", "/repo"), Some("/repo/src/main.rs".into()));
        assert_eq!(resolve_commit_dialog_file_path("src/main.rs", "/repo/"), Some("/repo/src/main.rs".into()));
        assert_eq!(resolve_commit_dialog_file_path("/abs/file.rs", "/repo"), Some("/abs/file.rs".into()));
    }
}
