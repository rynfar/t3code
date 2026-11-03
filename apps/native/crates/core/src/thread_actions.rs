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
    json!({ "type": "thread.visit", "commandId": command_id, "threadId": thread_id, "visitedAt": format_iso(visited_at) })
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

/// `buildThreadActionMenuItems`, without guest sharing and project promotion.
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
    if let Some((label, active)) = state.project_filter {
        let label = if active { "Show all projects".to_owned() } else { format!("Filter by {label}") };
        items.push(MenuItem::new(A::FilterByProject, label, Some("folder-tree")));
    }
    if state.supports.auto_settle_opt_out {
        let mut auto = MenuItem::new(A::AutoSettle, "Auto-settle behavior", Some("timer"));
        auto.children = vec![
            MenuItem { checked: Some(state.auto_settle_enabled), ..MenuItem::new(A::AutoSettleEnabled, "Enabled", None) },
            MenuItem { checked: Some(!state.auto_settle_enabled), ..MenuItem::new(A::AutoSettleDisabled, "Disabled", None) },
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

fn fnv1a64(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let (mut forward, mut reverse) = (0x811c_9dc5u32, 0x9dc5_811cu32);
    for index in 0..units.len() {
        forward = (forward ^ u32::from(units[index])).wrapping_mul(0x0100_0193);
        reverse = (reverse ^ u32::from(units[units.len() - 1 - index])).wrapping_mul(0x0100_0193);
    }
    format!("{forward:08x}{reverse:08x}")
}

fn is_context_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `toComposerContextId` (`packages/shared/src/composerContextReferences.ts`).
fn composer_context_id(producer_id: &str) -> String {
    if is_context_id(producer_id) {
        return producer_id.to_owned();
    }
    let mut slug = String::new();
    for c in producer_id.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    format!("{}-{}", if slug.is_empty() { "ctx" } else { &slug }, fnv1a64(producer_id))
}

/// `threadChipCopyTarget`'s plain text for Copy → Thread: a thread chip reference link.
pub fn thread_reference(title: &str, thread_id: &str) -> String {
    let cleaned: String = title.replace(['[', ']', '\\', '\r', '\n'], " ").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    let label = if cleaned.is_empty() { "thread".to_owned() } else { cleaned };
    format!("[{label}](t3-context://v1/thread/{})", composer_context_id(&format!("thread_{thread_id}")))
}

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
        assert!(!read_only.iter().find(|item| item.action == ThreadAction::Copy).unwrap().children[0].disabled);
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
}
