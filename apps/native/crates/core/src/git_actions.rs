//! Git quick actions: what the split button offers, why an action is unavailable, the default
//! branch confirmation, and the live progress of a running action. Ports
//! `apps/web/src/components/GitActionsControl.logic.ts`, the disabled reasons and inline success in
//! `GitActionsControl.tsx`, `requiresDefaultBranchConfirmation`
//! (`client-runtime/src/state/gitActions.ts`), the progress reducer in
//! `client-runtime/src/state/vcsAction.ts`, and each source control definition's change request
//! terms (`packages/source-control-*/src/shared/definition.ts`).

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::vcs::{VcsStatus, changes_totals};

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

#[derive(Debug, Clone, Default)]
pub struct RunRequest<'a> {
    pub commit_message: Option<&'a str>,
    pub feature_branch: bool,
    pub file_paths: Option<&'a [String]>,
    pub thread_id: Option<&'a str>,
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

#[cfg(test)]
mod tests;
