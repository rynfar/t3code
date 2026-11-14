//! Thread actions: the orchestration commands the sidebar sends
//! (`packages/client-runtime/src/operations/commands.ts`), the context menu model
//! (`apps/web/src/components/threadActionMenu.logic.ts`), and Undo
//! (`client-runtime/src/state/threadUndo.ts`, `apps/web/src/hooks/showThreadUndoNotice.ts`).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::sidebar::settled::SnoozePreset;
use crate::sidebar::time::{Ms, format_iso};

fn thread_command(kind: &str, command_id: &str, thread_id: &str) -> Value {
    json!({ "type": kind, "commandId": command_id, "threadId": thread_id })
}

pub fn archive(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.archive", command_id, thread_id)
}

pub fn unarchive(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.unarchive", command_id, thread_id)
}

pub fn delete(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.delete", command_id, thread_id)
}

pub fn settle(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.settle", command_id, thread_id)
}

/// `reason: "user"` keeps the thread active until real activity, suppressing auto-settle.
pub fn unsettle(command_id: &str, thread_id: &str) -> Value {
    json!({ "type": "thread.unsettle", "commandId": command_id, "threadId": thread_id, "reason": "user" })
}

pub fn pin(command_id: &str, thread_id: &str, order_key: Option<&str>) -> Value {
    let mut command = thread_command("thread.pin", command_id, thread_id);
    if let Some(order_key) = order_key {
        command["orderKey"] = json!(order_key);
    }
    command
}

/// `thread.pin.reorder`: moves a pinned thread to `order_key`.
pub fn reorder_pin(command_id: &str, thread_id: &str, order_key: &str) -> Value {
    let mut command = thread_command("thread.pin.reorder", command_id, thread_id);
    command["orderKey"] = json!(order_key);
    command
}

/// `thread.active.reorder`: moves an Active thread to `order_key`.
pub fn reorder_active(command_id: &str, thread_id: &str, order_key: &str) -> Value {
    let mut command = thread_command("thread.active.reorder", command_id, thread_id);
    command["orderKey"] = json!(order_key);
    command
}

pub fn unpin(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.unpin", command_id, thread_id)
}

pub fn snooze(command_id: &str, thread_id: &str, snoozed_until: Ms) -> Value {
    json!({ "type": "thread.snooze", "commandId": command_id, "threadId": thread_id, "snoozedUntil": format_iso(snoozed_until) })
}

pub fn unsnooze(command_id: &str, thread_id: &str) -> Value {
    json!({ "type": "thread.unsnooze", "commandId": command_id, "threadId": thread_id, "reason": "user" })
}

pub fn set_auto_settle(command_id: &str, thread_id: &str, enabled: bool) -> Value {
    json!({ "type": "thread.auto-settle.set", "commandId": command_id, "threadId": thread_id, "enabled": enabled })
}

pub fn visit(command_id: &str, thread_id: &str, visited_at: Ms) -> Value {
    visit_with(command_id, thread_id, visited_at, false, None)
}

pub fn visit_with(command_id: &str, thread_id: &str, visited_at: Ms, opened: bool, search_query: Option<&str>) -> Value {
    let mut command = json!({
        "type": "thread.visit",
        "commandId": command_id,
        "threadId": thread_id,
        "visitedAt": format_iso(visited_at),
    });
    if opened {
        command["opened"] = Value::Bool(true);
    }
    if let Some(query) = search_query.filter(|query| !query.is_empty()) {
        // Contracts cap `thread.visit.searchQuery` at 200 chars (`isMaxLength(200)`).
        let clamped: String = query.chars().take(200).collect();
        command["searchQuery"] = json!(clamped);
    }
    command
}

pub fn mark_unread(command_id: &str, thread_id: &str) -> Value {
    thread_command("thread.mark-unread", command_id, thread_id)
}

pub fn rename(command_id: &str, thread_id: &str, title: &str) -> Value {
    json!({ "type": "thread.metadata.update", "commandId": command_id, "threadId": thread_id, "title": title })
}

pub fn regenerate_title(command_id: &str, thread_id: &str) -> Value {
    json!({ "type": "thread.metadata.update", "commandId": command_id, "threadId": thread_id, "regenerateTitle": true })
}

/// Settled result of a `thread.metadata.update { regenerateTitle: true }`
/// command observed via `ThreadShell.last_title_regeneration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRegenerationOutcome {
    pub status: String,
    pub message: Option<String>,
}

/// Command-plus-outcome result for `title_regeneration_toast`
/// (`apps/web/src/components/threadTitleRegenerationToast.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleRegenerationResult {
    Success(Option<TitleRegenerationOutcome>),
    Failure(String),
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRegenerationToast {
    pub is_error: bool,
    pub title: String,
    pub description: Option<String>,
}

/// Returns `None` while regeneration is still pending, `Some(None)` if the
/// snapshot moved past `request_sequence` and either dropped the thread or
/// cleared `titleRegeneration` without recording `lastTitleRegeneration` for
/// `request_id`, and `Some(Some(outcome))` when settled.
pub fn settled_title_regeneration_outcome(
    snapshot_sequence: u64,
    request_sequence: u64,
    request_id: &str,
    last: Option<&crate::shell::LastTitleRegeneration>,
    active: Option<&Value>,
    thread_exists: bool,
) -> Option<Option<TitleRegenerationOutcome>> {
    if snapshot_sequence < request_sequence {
        return None;
    }
    if !thread_exists {
        return Some(None);
    }
    if let Some(last) = last
        && last.request_id == request_id
    {
        return Some(Some(TitleRegenerationOutcome {
            status: last.status.clone(),
            message: last.message.clone(),
        }));
    }
    let still_active = active
        .and_then(|value| value.get("requestId"))
        .and_then(Value::as_str)
        == Some(request_id);
    if still_active { None } else { Some(None) }
}

pub fn title_regeneration_toast(
    results: &[TitleRegenerationResult],
) -> Option<TitleRegenerationToast> {
    let failures: Vec<&str> = results
        .iter()
        .filter_map(|result| match result {
            TitleRegenerationResult::Failure(message) => Some(message.as_str()),
            TitleRegenerationResult::Interrupted => None,
            TitleRegenerationResult::Success(Some(outcome)) if outcome.status == "failed" => {
                Some(outcome.message.as_deref().unwrap_or("Title generation failed."))
            }
            TitleRegenerationResult::Success(_) => None,
        })
        .collect();
    if !failures.is_empty() {
        return Some(TitleRegenerationToast {
            is_error: true,
            title: if results.len() == 1 {
                "Failed to regenerate thread title".to_owned()
            } else {
                format!("Failed to regenerate {} of {} thread titles", failures.len(), results.len())
            },
            description: Some(failures[0].to_owned()),
        });
    }
    let kept = results
        .iter()
        .filter(|result| {
            matches!(
                result,
                TitleRegenerationResult::Success(Some(outcome)) if outcome.status == "unchanged"
            )
        })
        .count();
    if kept == 0 {
        return None;
    }
    if results.len() == 1 {
        return Some(TitleRegenerationToast {
            is_error: false,
            title: "Title kept".to_owned(),
            description: Some("The current title already fits this thread.".to_owned()),
        });
    }
    Some(TitleRegenerationToast {
        is_error: false,
        title: if kept == results.len() {
            "Titles kept".to_owned()
        } else {
            format!("Kept {kept} of {} titles", results.len())
        },
        description: Some(if kept == results.len() {
            "The current titles already fit these threads.".to_owned()
        } else {
            "Those titles already fit their threads.".to_owned()
        }),
    })
}

/// What a rename commit does (`commitThreadRename` in `Sidebar.tsx`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    Empty,
    Unchanged,
    Rename(String),
}

pub fn resolve_rename(title: &str, original: &str) -> RenameOutcome {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        RenameOutcome::Empty
    } else if trimmed == original {
        RenameOutcome::Unchanged
    } else {
        RenameOutcome::Rename(trimmed.to_owned())
    }
}

/// The delete confirmation copy (`useThreadActionMenu.ts`).
pub fn delete_confirmation(title: &str) -> (String, &'static str) {
    (format!("Delete thread \"{title}\"?"), "This permanently clears conversation history for this thread.")
}

/// The unpin confirmation copy (`requestThreadUnpinConfirmation` in `useThreadActions.ts`).
pub fn unpin_confirmation(title: &str) -> (String, &'static str) {
    (format!("Unpin thread \"{title}\"?"), "This will move the thread out of your pinned section.")
}

/// The archive confirmation copy (`useThreadActionMenu.ts`); the web asks with a title only.
pub fn archive_confirmation(title: &str) -> String {
    format!("Archive thread \"{title}\"?")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThreadAction {
    Find,
    NewThreadOnBranch,
    FilterByProject,
    ProjectSettings,
    Pin,
    Unpin,
    Settle,
    Unsettle,
    AutoSettle,
    AutoSettleEnabled,
    AutoSettleDisabled,
    Snooze,
    SnoozePreset(String),
    SnoozeCustom,
    Unsnooze,
    Rename,
    RegenerateTitle,
    MarkUnread,
    ShareGuest,
    TurnIntoProject,
    Copy,
    CopyPath,
    CopyBranch,
    CopyThread,
    CopyThreadId,
    Archive,
    Delete,
}

impl ThreadAction {
    /// `threadActionRequiresOperate`: navigation, read markers and copying stay available.
    pub fn requires_operate(&self) -> bool {
        !matches!(
            self,
            ThreadAction::Find
                | ThreadAction::NewThreadOnBranch
                | ThreadAction::ProjectSettings
                | ThreadAction::MarkUnread
                | ThreadAction::ShareGuest
                | ThreadAction::Copy
                | ThreadAction::CopyPath
                | ThreadAction::CopyBranch
                | ThreadAction::CopyThread
                | ThreadAction::CopyThreadId
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem<A = ThreadAction> {
    pub action: A,
    pub label: String,
    /// Lucide icon name.
    pub icon: Option<&'static str>,
    pub disabled: bool,
    pub destructive: bool,
    pub separator_before: bool,
    pub checked: Option<bool>,
    pub children: Vec<MenuItem<A>>,
}

impl<A> MenuItem<A> {
    pub fn new(action: A, label: impl Into<String>, icon: Option<&'static str>) -> MenuItem<A> {
        MenuItem { action, label: label.into(), icon, disabled: false, destructive: false, separator_before: false, checked: None, children: Vec::new() }
    }

    /// The same menu with each action wrapped, so one menu view can show several kinds.
    pub fn map<B>(self, wrap: &impl Fn(A) -> B) -> MenuItem<B> {
        MenuItem {
            action: wrap(self.action),
            label: self.label,
            icon: self.icon,
            disabled: self.disabled,
            destructive: self.destructive,
            separator_before: self.separator_before,
            checked: self.checked,
            children: self.children.into_iter().map(|child| child.map(wrap)).collect(),
        }
    }

    fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Supports {
    pub settlement: bool,
    pub auto_settle_opt_out: bool,
    pub snooze: bool,
    pub pinning: bool,
    pub title_regeneration: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadActionMenuState<'a> {
    pub can_operate: bool,
    pub can_find: bool,
    pub can_share_with_guest: bool,
    pub can_turn_into_project: bool,
    pub branch: Option<&'a str>,
    pub project_filter: Option<(&'a str, bool)>,
    pub is_pinned: bool,
    pub is_settled: bool,
    pub auto_settle_enabled: bool,
    pub is_snoozed: bool,
    pub can_snooze_now: bool,
    pub is_regenerating_title: bool,
    pub is_running: bool,
    pub supports: Supports,
    pub snooze_presets: &'a [SnoozePreset],
    /// Surfaces this client has yet to build stay out of the menu instead of doing nothing.
    pub available: Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Available {
    pub new_thread_on_branch: bool,
    pub custom_snooze: bool,
    pub project_settings: bool,
}

impl Available {
    pub const ALL: Available = Available { new_thread_on_branch: true, custom_snooze: true, project_settings: true };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectAction {
    NewThread,
    CopyPath,
    Pin,
    Unpin,
    Settings,
}

/// The project menu (`handleProjectFolderContextMenu` in `Sidebar.tsx`).
pub fn build_project_menu(pinned: bool, can_pin: bool) -> Vec<MenuItem<ProjectAction>> {
    let mut items = vec![MenuItem::new(ProjectAction::NewThread, "New thread", None), MenuItem::new(ProjectAction::CopyPath, "Copy path", None)];
    if can_pin {
        items.push(if pinned {
            MenuItem::new(ProjectAction::Unpin, "Unpin project", Some("pin-off"))
        } else {
            MenuItem::new(ProjectAction::Pin, "Pin project", Some("pin"))
        });
    }
    items.push(MenuItem::new(ProjectAction::Settings, "Project settings", Some("settings")));
    items
}

/// `buildThreadActionMenuItems`.
pub fn build_menu(state: &ThreadActionMenuState) -> Vec<MenuItem> {
    use ThreadAction as A;
    let mut items = Vec::new();
    if state.can_find {
        items.push(MenuItem::new(A::Find, "Find in thread", Some("text-search")));
    }
    if let Some(branch) = state.branch.filter(|_| state.available.new_thread_on_branch) {
        items.push(MenuItem::new(A::NewThreadOnBranch, format!("New thread on {branch}"), Some("message-square-plus")));
    }
    if state.supports.pinning {
        items.push(if state.is_pinned {
            MenuItem::new(A::Unpin, "Unpin thread", Some("pin-off"))
        } else {
            MenuItem::new(A::Pin, "Pin thread", Some("pin"))
        });
    }
    if state.supports.settlement {
        items.push(if state.is_settled {
            MenuItem::new(A::Unsettle, "Un-settle thread", Some("circle-check"))
        } else {
            MenuItem::new(A::Settle, "Settle thread", Some("circle-check"))
        });
    }
    if state.supports.snooze {
        if state.is_snoozed {
            items.push(MenuItem::new(A::Unsnooze, "Wake thread", Some("clock")));
        } else {
            let mut snooze = MenuItem::new(A::Snooze, "Snooze", Some("clock")).disabled(!state.can_snooze_now);
            snooze.children = state
                .snooze_presets
                .iter()
                .map(|preset| MenuItem::new(A::SnoozePreset(preset.id.clone()), format!("{} ({})", preset.label, preset.when_label), None))
                .collect();
            if state.available.custom_snooze {
                let custom = MenuItem::new(A::SnoozeCustom, "Custom…", None);
                snooze.children.push(if state.snooze_presets.is_empty() { custom } else { custom.separated() });
            }
            items.push(snooze);
        }
    }
    items.push(MenuItem::new(A::Rename, "Rename thread", Some("pencil")).separated());
    if state.supports.title_regeneration {
        let label = if state.is_regenerating_title { "Regenerating…" } else { "Regenerate title" };
        items.push(MenuItem::new(A::RegenerateTitle, label, Some("refresh-cw")).disabled(state.is_regenerating_title));
    }
    items.push(MenuItem::new(A::MarkUnread, "Mark unread", Some("mail-open")));
    if state.can_share_with_guest {
        items.push(MenuItem::new(A::ShareGuest, "Share with guest", Some("user-plus")));
    }
    if let Some((label, active)) = state.project_filter {
        let label = if active { "Show all projects".to_owned() } else { format!("Filter by {label}") };
        items.push(MenuItem::new(A::FilterByProject, label, Some("folder-tree")));
    }
    if state.can_turn_into_project {
        items.push(MenuItem::new(A::TurnIntoProject, "Turn into project", Some("folder-plus")).disabled(state.is_running));
    }
    if state.supports.auto_settle_opt_out {
        let mut auto = MenuItem::new(A::AutoSettle, "Auto-settle behavior", Some("timer"));
        auto.children = vec![
            MenuItem { checked: Some(state.auto_settle_enabled), ..MenuItem::new(A::AutoSettleEnabled, "Follow settings", None) },
            MenuItem { checked: Some(!state.auto_settle_enabled), ..MenuItem::new(A::AutoSettleDisabled, "Never", None) },
        ];
        items.push(auto);
    }
    let mut copy = MenuItem::new(A::Copy, "Copy", Some("copy")).separated();
    copy.children.push(MenuItem::new(A::CopyPath, "Path", Some("folder")));
    if state.branch.is_some() {
        copy.children.push(MenuItem::new(A::CopyBranch, "Branch", Some("git-branch")));
    }
    copy.children.push(MenuItem::new(A::CopyThread, "Thread", Some("message-square")));
    copy.children.push(MenuItem::new(A::CopyThreadId, "Thread ID", Some("hash")));
    items.push(copy);
    if state.available.project_settings {
        items.push(MenuItem::new(A::ProjectSettings, "Project settings", Some("settings")));
    }
    items.push(MenuItem::new(A::Archive, "Archive thread", Some("archive")).disabled(state.is_running).separated());
    items.push(MenuItem { destructive: true, ..MenuItem::new(A::Delete, "Delete", Some("trash")) });
    if state.can_operate {
        return items;
    }
    items
        .into_iter()
        .map(|mut item| {
            if item.action.requires_operate() {
                item.disabled = true;
                for child in &mut item.children {
                    child.disabled = true;
                }
            }
            item
        })
        .collect()
}

pub mod guest_share;
pub(crate) mod thread_reference;
pub use thread_reference::thread_reference;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoStep {
    RestoreDraft,
    Unsettle,
    Unsnooze,
    Pin { order_key: Option<String> },
    Snooze { snoozed_until: Ms },
    Unarchive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UndoKind {
    Discard,
    Settle,
    Snooze,
    Pin,
    Archive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFor {
    Settle,
    Snooze,
    Unpin,
}

/// `planThreadUndo`. Pinning clears snooze on the server, so a snooze is restored after the pin.
pub fn plan_undo(action: PlanFor, pinned: bool, pin_order_key: Option<&str>, snoozed_until: Option<Ms>) -> Vec<UndoStep> {
    if action == PlanFor::Snooze {
        return vec![UndoStep::Unsnooze];
    }
    let pin = UndoStep::Pin { order_key: pin_order_key.map(str::to_owned) };
    let snooze = snoozed_until.map(|snoozed_until| UndoStep::Snooze { snoozed_until });
    let mut plan = Vec::new();
    if action == PlanFor::Unpin {
        plan.push(pin);
    } else {
        plan.push(UndoStep::Unsettle);
        if pinned {
            plan.push(pin);
        }
    }
    plan.extend(snooze);
    plan
}

/// `executeThreadUndo` drops a snooze restore that already expired.
pub fn runnable_step(step: &UndoStep, now: Ms) -> bool {
    !matches!(step, UndoStep::Snooze { snoozed_until } if *snoozed_until <= now)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoAction {
    Discarded,
    Settled,
    Snoozed,
    Unpinned,
    Archived,
}

impl UndoAction {
    pub fn label(self) -> &'static str {
        match self {
            UndoAction::Discarded => "Discarded",
            UndoAction::Settled => "Settled",
            UndoAction::Snoozed => "Snoozed",
            UndoAction::Unpinned => "Unpinned",
            UndoAction::Archived => "Archived",
        }
    }
}

pub const UNDO_NOTICE_MS: Ms = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    kind: UndoKind,
    thread_id: String,
    token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveUndo {
    action: UndoAction,
    claim: Claim,
    plan: Vec<UndoStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoNotice {
    pub action: UndoAction,
    pub count: usize,
}

impl UndoNotice {
    pub fn announcement_summary(&self) -> String {
        let noun = if self.action == UndoAction::Discarded { "draft" } else { "thread" };
        format!("{} {} {noun}{}", self.action.label(), self.count, if self.count == 1 { "" } else { "s" })
    }

    /// The compact footer text: "Settled" or "Settled 3".
    pub fn summary(&self) -> String {
        if self.count == 1 { self.action.label().to_owned() } else { format!("{} {}", self.action.label(), self.count) }
    }
}

/// Undo claims and the live notice. Sidebar, header and menu actions share one store so they
/// invalidate each other, as the module-level maps do on the web.
#[derive(Debug, Default)]
pub struct ThreadUndoStore {
    current: HashMap<(UndoKind, String), u64>,
    next_token: u64,
    live: Vec<LiveUndo>,
    expires_at: Option<Ms>,
}

impl ThreadUndoStore {
    pub fn begin(&mut self, kind: UndoKind, thread_id: &str) -> Claim {
        self.next_token += 1;
        self.current.insert((kind, thread_id.to_owned()), self.next_token);
        Claim { kind, thread_id: thread_id.to_owned(), token: self.next_token }
    }

    pub fn is_current(&self, claim: &Claim) -> bool {
        self.current.get(&(claim.kind, claim.thread_id.clone())) == Some(&claim.token)
    }

    pub fn finish(&mut self, claim: &Claim) {
        if self.is_current(claim) {
            self.current.remove(&(claim.kind, claim.thread_id.clone()));
        }
        self.refresh();
    }

    pub fn invalidate(&mut self, kind: UndoKind, thread_id: &str) {
        self.current.remove(&(kind, thread_id.to_owned()));
        self.refresh();
    }

    /// Archiving or deleting ends a thread's place in the list, so its placement undos expire.
    pub fn expire_placement(&mut self, thread_id: &str, include_archive: bool) {
        for kind in [UndoKind::Settle, UndoKind::Snooze, UndoKind::Pin] {
            self.current.remove(&(kind, thread_id.to_owned()));
        }
        if include_archive {
            self.current.remove(&(UndoKind::Archive, thread_id.to_owned()));
        }
        self.refresh();
    }

    pub fn show(&mut self, action: UndoAction, claim: Claim, plan: Vec<UndoStep>, now: Ms) {
        if !self.is_current(&claim) {
            return;
        }
        self.live.push(LiveUndo { action, claim, plan });
        self.expires_at = Some(now + UNDO_NOTICE_MS);
        self.refresh();
    }

    fn refresh(&mut self) {
        let current = &self.current;
        self.live.retain(|entry| current.get(&(entry.claim.kind, entry.claim.thread_id.clone())) == Some(&entry.claim.token));
        if self.live.is_empty() {
            self.expires_at = None;
        }
    }

    pub fn notice(&self) -> Option<UndoNotice> {
        let latest = self.live.last()?;
        let count = self.live.iter().rev().take_while(|entry| entry.action == latest.action).count();
        Some(UndoNotice { action: latest.action, count })
    }

    pub fn expires_at(&self) -> Option<Ms> {
        self.expires_at
    }

    /// Ends every claim once the notice times out.
    pub fn expire(&mut self, now: Ms) -> bool {
        if self.expires_at.is_none_or(|at| at > now) {
            return false;
        }
        for entry in std::mem::take(&mut self.live) {
            self.current.remove(&(entry.claim.kind, entry.claim.thread_id));
        }
        self.expires_at = None;
        true
    }

    /// Takes the latest group of same-action undos: `(thread id, plan)` for each, to run now.
    pub fn take_latest_group(&mut self) -> Vec<(String, Vec<UndoStep>)> {
        let Some(action) = self.live.last().map(|entry| entry.action) else { return Vec::new() };
        let split = self.live.iter().rposition(|entry| entry.action != action).map_or(0, |index| index + 1);
        let group: Vec<LiveUndo> = self.live.drain(split..).collect();
        for entry in &group {
            self.current.remove(&(entry.claim.kind, entry.claim.thread_id.clone()));
        }
        self.refresh();
        group.into_iter().map(|entry| (entry.claim.thread_id, entry.plan)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supports() -> Supports {
        Supports { settlement: true, auto_settle_opt_out: false, snooze: true, pinning: true, title_regeneration: true }
    }

    fn state(presets: &[SnoozePreset]) -> ThreadActionMenuState<'_> {
        ThreadActionMenuState {
            can_operate: true,
            can_find: false,
            can_share_with_guest: false,
            can_turn_into_project: false,
            branch: Some("feature/x"),
            project_filter: None,
            is_pinned: false,
            is_settled: false,
            auto_settle_enabled: true,
            is_snoozed: false,
            can_snooze_now: true,
            is_regenerating_title: false,
            is_running: false,
            supports: supports(),
            snooze_presets: presets,
            available: Available::ALL,
        }
    }

    #[test]
    fn copied_thread_reference_uses_the_hosted_contract() {
        assert_eq!(
            thread_reference("Fix chips", Some("env-1"), "thread-1")
                .unwrap()
                .text,
            "https://app.t3.codes/threads/env-1/thread-1"
        );
        for (environment, thread, expected) in [
            (
                "env.1",
                "thread:delegated/mcp%3A1.",
                "env%2E1/thread%3Adelegated%2Fmcp%253A1%2E",
            ),
            (
                "测试",
                "é?!~*'()#&=",
                "%E6%B5%8B%E8%AF%95/%C3%A9%3F%21%7E%2A%27%28%29%23%26%3D",
            ),
        ] {
            let target = thread_reference("Fix [chips]\nnow", Some(environment), thread).unwrap();
            assert_eq!(
                target.text,
                format!("https://app.t3.codes/threads/{expected}")
            );
            let fragment = crate::composer::records::decode_fragment(&target.fragment).unwrap();
            assert_eq!(fragment["source"]["environmentId"], environment);
            assert_eq!(fragment["records"][0]["threadId"], thread);
            assert_eq!(fragment["records"][0]["environmentId"], environment);
            assert_eq!(fragment["records"][0]["title"], "Fix chips now");
        }
        let unicode = thread_reference(&"😀".repeat(150), Some("e"), "t").unwrap();
        assert_eq!(unicode.description.encode_utf16().count(), 200);
        assert!(crate::composer::records::decode_fragment(&unicode.fragment).is_some());
        for environment in [None, Some(""), Some(" "), Some(" env")] {
            assert!(thread_reference("x", environment, "t").is_none());
        }
        for id in ["", " ", " t"] {
            assert!(thread_reference("x", Some("e"), id).is_none());
        }
        assert_ne!(
            thread_reference("x", Some("a"), "same").unwrap().text,
            thread_reference("x", Some("b"), "same").unwrap().text
        );
    }

    #[test]
    fn commands_match_the_contract_shapes() {
        assert_eq!(pin("c", "t", None), json!({"type": "thread.pin", "commandId": "c", "threadId": "t"}));
        assert_eq!(pin("c", "t", Some("m"))["orderKey"], "m");
        assert_eq!(unsettle("c", "t")["reason"], "user");
        assert_eq!(unsnooze("c", "t")["reason"], "user");
        assert_eq!(snooze("c", "t", 0)["snoozedUntil"], "1970-01-01T00:00:00.000Z");
        assert_eq!(rename("c", "t", "New")["type"], "thread.metadata.update");
        assert_eq!(regenerate_title("c", "t")["regenerateTitle"], true);
        assert_eq!(set_auto_settle("c", "t", false)["enabled"], false);
        assert_eq!(mark_unread("c", "t")["type"], "thread.mark-unread");
        assert_eq!(reorder_pin("c", "t", "m"), json!({"type": "thread.pin.reorder", "commandId": "c", "threadId": "t", "orderKey": "m"}));
        assert_eq!(reorder_active("c", "t", "m")["type"], "thread.active.reorder");
        assert_eq!(visit("c", "t", 0)["visitedAt"], "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn menu_lists_the_web_items_in_order() {
        let presets = [SnoozePreset { id: "after:60".into(), label: "In 1 hour".into(), when_label: "3:00 PM".into(), snoozed_until: 1 }];
        let items = build_menu(&state(&presets));
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(
            labels,
            ["New thread on feature/x", "Pin thread", "Settle thread", "Snooze", "Rename thread", "Regenerate title", "Mark unread", "Copy", "Project settings", "Archive thread", "Delete"]
        );
        let snooze = &items[3];
        assert_eq!(snooze.children[0].label, "In 1 hour (3:00 PM)");
        assert_eq!(snooze.children[1].action, ThreadAction::SnoozeCustom);
        assert!(snooze.children[1].separator_before);
        let copy: Vec<&str> = items[7].children.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(copy, ["Path", "Branch", "Thread", "Thread ID"]);
        assert!(items[10].destructive);
    }

    #[test]
    fn menu_shows_reverse_actions_and_read_only_disables_writes() {
        let reversed = build_menu(&ThreadActionMenuState { is_pinned: true, is_settled: true, is_snoozed: true, is_running: true, ..state(&[]) });
        let labels: Vec<&str> = reversed.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"Unpin thread") && labels.contains(&"Un-settle thread") && labels.contains(&"Wake thread"));
        assert!(reversed.iter().find(|item| item.action == ThreadAction::Archive).unwrap().disabled);
        let read_only = build_menu(&ThreadActionMenuState { can_operate: false, ..state(&[]) });
        assert!(read_only.iter().find(|item| item.action == ThreadAction::Pin).unwrap().disabled);
        assert!(!read_only.iter().find(|item| item.action == ThreadAction::MarkUnread).unwrap().disabled);
        assert!(!ThreadAction::MarkUnread.requires_operate());
        assert!(ThreadAction::FilterByProject.requires_operate());
        assert!(!read_only.iter().find(|item| item.action == ThreadAction::Copy).unwrap().children[0].disabled);
    }

    #[test]
    fn optional_actions_keep_web_order_and_independent_grants() {
        let items = build_menu(&ThreadActionMenuState { can_find:true, can_share_with_guest:true, can_turn_into_project:true, project_filter:Some(("Repo", false)), supports:Supports { auto_settle_opt_out:true, ..supports() }, ..state(&[]) });
        let actions: Vec<_> = items.iter().map(|item| item.action.clone()).collect();
        assert_eq!(actions, [ThreadAction::Find, ThreadAction::NewThreadOnBranch, ThreadAction::Pin, ThreadAction::Settle, ThreadAction::Snooze, ThreadAction::Rename, ThreadAction::RegenerateTitle, ThreadAction::MarkUnread, ThreadAction::ShareGuest, ThreadAction::FilterByProject, ThreadAction::TurnIntoProject, ThreadAction::AutoSettle, ThreadAction::Copy, ThreadAction::ProjectSettings, ThreadAction::Archive, ThreadAction::Delete]);
        let read_only = build_menu(&ThreadActionMenuState { can_operate:false, can_share_with_guest:true, can_turn_into_project:true, supports:Supports {auto_settle_opt_out:true, ..supports()}, ..state(&[]) });
        assert!(!read_only.iter().find(|item| item.action == ThreadAction::ShareGuest).unwrap().disabled);
        assert!(read_only.iter().find(|item| item.action == ThreadAction::TurnIntoProject).unwrap().disabled);
        assert!(read_only.iter().find(|item| item.action == ThreadAction::AutoSettle).unwrap().children.iter().all(|child| child.disabled));
    }

    #[test]
    fn undo_plans_restore_pin_then_snooze() {
        assert_eq!(plan_undo(PlanFor::Snooze, false, None, None), [UndoStep::Unsnooze]);
        assert_eq!(
            plan_undo(PlanFor::Settle, true, Some("k"), Some(9)),
            [UndoStep::Unsettle, UndoStep::Pin { order_key: Some("k".into()) }, UndoStep::Snooze { snoozed_until: 9 }]
        );
        assert_eq!(plan_undo(PlanFor::Unpin, true, None, None), [UndoStep::Pin { order_key: None }]);
        assert!(!runnable_step(&UndoStep::Snooze { snoozed_until: 5 }, 5));
    }

    #[test]
    fn undo_notice_groups_the_latest_action_and_expires() {
        let mut store = ThreadUndoStore::default();
        for id in ["a", "b"] {
            let claim = store.begin(UndoKind::Settle, id);
            store.show(UndoAction::Settled, claim, vec![UndoStep::Unsettle], 0);
        }
        assert_eq!(store.notice(), Some(UndoNotice { action: UndoAction::Settled, count: 2 }));
        assert_eq!(store.notice().unwrap().summary(), "Settled 2");
        assert_eq!(store.notice().unwrap().announcement_summary(), "Settled 2 threads");
        store.invalidate(UndoKind::Settle, "a");
        assert_eq!(store.notice().map(|notice| notice.count), Some(1));
        let group = store.take_latest_group();
        assert_eq!(group, [("b".to_owned(), vec![UndoStep::Unsettle])]);
        assert_eq!(store.notice(), None);
        let claim = store.begin(UndoKind::Archive, "c");
        store.show(UndoAction::Archived, claim, vec![UndoStep::Unarchive], 100);
        assert!(!store.expire(100 + UNDO_NOTICE_MS - 1));
        assert!(store.expire(100 + UNDO_NOTICE_MS));
        assert_eq!(store.notice(), None);
    }

    #[test]
    fn a_stale_claim_shows_nothing() {
        let mut store = ThreadUndoStore::default();
        let first = store.begin(UndoKind::Pin, "a");
        let _second = store.begin(UndoKind::Pin, "a");
        store.show(UndoAction::Unpinned, first, Vec::new(), 0);
        assert_eq!(store.notice(), None);
    }

    #[test]
    fn project_menu_offers_pin_only_where_the_server_can() {
        let labels = |items: Vec<MenuItem<ProjectAction>>| items.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert_eq!(labels(build_project_menu(false, true)), ["New thread", "Copy path", "Pin project", "Project settings"]);
        assert_eq!(labels(build_project_menu(true, true)), ["New thread", "Copy path", "Unpin project", "Project settings"]);
        assert_eq!(labels(build_project_menu(true, false)), ["New thread", "Copy path", "Project settings"]);
    }

    #[test]
    fn rename_trims_and_rejects_empty_titles() {
        assert_eq!(resolve_rename("  ", "x"), RenameOutcome::Empty);
        assert_eq!(resolve_rename(" x ", "x"), RenameOutcome::Unchanged);
        assert_eq!(resolve_rename(" y ", "x"), RenameOutcome::Rename("y".into()));
    }

    #[test]
    fn visit_with_encodes_opened_and_clamps_search_query() {
        let long_query = "q".repeat(250);
        let cmd = visit_with("c1", "t1", 1_700_000_000_000, true, Some(&long_query));
        assert_eq!(cmd["opened"], true);
        assert_eq!(cmd["searchQuery"].as_str().map(|s| s.chars().count()), Some(200));
        let plain = visit("c2", "t1", 1_700_000_000_000);
        assert!(plain.get("opened").is_none());
        assert!(plain.get("searchQuery").is_none());
    }

    #[test]
    fn auto_settle_menu_uses_follow_settings_and_never_labels() {
        let items = build_menu(&ThreadActionMenuState {
            supports: Supports { auto_settle_opt_out: true, ..supports() },
            ..state(&[])
        });
        let auto = items.iter().find(|item| item.action == ThreadAction::AutoSettle).unwrap();
        let child_labels: Vec<&str> = auto.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(child_labels, ["Follow settings", "Never"]);
    }

    #[test]
    fn title_regeneration_outcome_waits_for_sequence_and_reports_outcomes() {
        let active = Some(json!({ "requestId": "req-1" }));
        let last = crate::shell::LastTitleRegeneration {
            request_id: "req-1".into(),
            status: "unchanged".into(),
            message: None,
            completed_at: "2026-11-14T00:00:00.000Z".into(),
        };
        assert_eq!(settled_title_regeneration_outcome(4, 5, "req-1", Some(&last), active.as_ref(), true), None);
        assert_eq!(settled_title_regeneration_outcome(5, 5, "req-1", None, active.as_ref(), true), None);
        assert_eq!(settled_title_regeneration_outcome(5, 5, "req-1", None, None, true), Some(None));
        assert_eq!(settled_title_regeneration_outcome(5, 5, "req-1", None, active.as_ref(), false), Some(None));
        assert_eq!(
            settled_title_regeneration_outcome(5, 5, "req-1", Some(&last), None, true),
            Some(Some(TitleRegenerationOutcome { status: "unchanged".into(), message: None }))
        );
    }

    #[test]
    fn title_regeneration_toast_formats_failures_and_kept_titles() {
        assert_eq!(
            title_regeneration_toast(&[TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                status: "updated".into(),
                message: None,
            }))]),
            None
        );
        assert_eq!(
            title_regeneration_toast(&[TitleRegenerationResult::Interrupted]),
            None
        );
        assert_eq!(
            title_regeneration_toast(&[TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                status: "unchanged".into(),
                message: None,
            }))]),
            Some(TitleRegenerationToast {
                is_error: false,
                title: "Title kept".into(),
                description: Some("The current title already fits this thread.".into()),
            })
        );
        assert_eq!(
            title_regeneration_toast(&[
                TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                    status: "unchanged".into(),
                    message: None,
                })),
                TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                    status: "unchanged".into(),
                    message: None,
                })),
            ]),
            Some(TitleRegenerationToast {
                is_error: false,
                title: "Titles kept".into(),
                description: Some("The current titles already fit these threads.".into()),
            })
        );
        assert_eq!(
            title_regeneration_toast(&[
                TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                    status: "unchanged".into(),
                    message: None,
                })),
                TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                    status: "updated".into(),
                    message: None,
                })),
            ]),
            Some(TitleRegenerationToast {
                is_error: false,
                title: "Kept 1 of 2 titles".into(),
                description: Some("Those titles already fit their threads.".into()),
            })
        );
        assert_eq!(
            title_regeneration_toast(&[
                TitleRegenerationResult::Success(Some(TitleRegenerationOutcome {
                    status: "failed".into(),
                    message: Some("Model error".into()),
                })),
                TitleRegenerationResult::Failure("Timed out".into()),
            ]),
            Some(TitleRegenerationToast {
                is_error: true,
                title: "Failed to regenerate 2 of 2 thread titles".into(),
                description: Some("Model error".into()),
            })
        );
    }
}
