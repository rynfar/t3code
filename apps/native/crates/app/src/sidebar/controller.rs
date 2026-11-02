//! Sidebar state on `AppState`: the derived model, shelf toggles, the Working-shelf tracker,
//! Undo, the open thread's visit watermark, and the thread actions the menu, row buttons,
//! keybindings and palette share (`apps/web/src/hooks/useThreadActions.ts`).

use std::sync::Arc;
use std::time::Duration;

use gpui::{ClipboardItem, Context, Task};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_core::sidebar::inbox::InboxReturnTracker;
use t3_core::sidebar::settled::{self, DEFAULT_SNOOZE_PRESETS, SnoozePreset};
use t3_core::sidebar::sort::top_of_pinned_order_key;
use t3_core::sidebar::time::{LocalTime, Ms, Zone};
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::sidebar::thread::same_apart_from_streaming;
use t3_core::sidebar::visits::{VisitDecision, VisitTracker, VisitedShell};
use t3_core::sidebar::{ModelInput, Shelf, ShelfState, SidebarModel, SidebarSettings};
use t3_core::thread_actions::{
    self as actions, PlanFor, RenameOutcome, ThreadAction, ThreadUndoStore, UndoAction, UndoKind, UndoStep,
};

use crate::app_state::{AppEvent, AppState, Phase};
use crate::toast::{Toast, ToastKind};
use t3_client::supervisor::ConnectionStatus;

/// A thread delta that only moves streaming fields; the sidebar skips its rebuild.
pub fn is_streaming_tick(shell: &ShellState, item: &ShellStreamItem) -> bool {
    match item {
        ShellStreamItem::ThreadUpdated { location, thread, .. } if location == "active" => {
            shell.thread(&thread.id).is_some_and(|previous| same_apart_from_streaming(previous, thread))
        }
        _ => false,
    }
}

/// What this environment lets the menu offer. The web reads these from the server config and the
/// session's scopes (`useThreadActionMenu.ts`); until the native client subscribes to the server
/// config, the defaults assume a current server and an operator session, and keep auto-settle
/// opt-out (a newer capability) hidden. The server-config subscription should set this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadCapabilities {
    pub can_operate: bool,
    pub config_loaded: bool,
    pub auto_settle_opt_out: bool,
}

impl Default for ThreadCapabilities {
    fn default() -> Self {
        ThreadCapabilities { can_operate: true, config_loaded: true, auto_settle_opt_out: false }
    }
}

pub fn now_ms() -> Ms {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as Ms).unwrap_or(0)
}

/// The OS time zone, including DST for any instant.
pub struct SystemZone;

impl Zone for SystemZone {
    #[cfg(unix)]
    fn offset_seconds_at(&self, utc_ms: Ms) -> i64 {
        let seconds = utc_ms.div_euclid(1000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` writes only into `tm`, which outlives the call.
        let ok = unsafe { !libc::localtime_r(&seconds, &mut tm).is_null() };
        if ok { tm.tm_gmtoff as i64 } else { 0 }
    }

    #[cfg(not(unix))]
    fn offset_seconds_at(&self, _utc_ms: Ms) -> i64 {
        0
    }
}

pub fn time_label(time: LocalTime) -> String {
    settled::default_time_label(time)
}

#[derive(Default)]
pub struct SidebarController {
    pub model: Arc<SidebarModel>,
    pub shelves: ShelfState,
    pub settings: SidebarSettings,
    pub undo: ThreadUndoStore,
    pub capabilities: ThreadCapabilities,
    pub collapsed: bool,
    tracker: InboxReturnTracker,
    visit: Option<VisitTracker>,
    wake_timer: Option<Task<()>>,
    visit_timer: Option<Task<()>>,
    undo_timer: Option<Task<()>>,
}

fn connected(phase: &Phase) -> bool {
    matches!(phase, Phase::Online(ConnectionStatus::Connected))
}

impl AppState {
    /// Re-derives the sidebar from the shell. Cheap enough per batch; rows notify only the sidebar.
    pub(crate) fn rebuild_sidebar(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let model = match &self.shell.snapshot {
            Some(snapshot) => SidebarModel::build(
                &ModelInput {
                    snapshot,
                    settings: &self.sidebar.settings,
                    shelves: &self.sidebar.shelves,
                    selected_thread_id: self.selected_thread.as_deref(),
                    project_scope: None,
                    connected: connected(&self.phase),
                    now,
                },
                &mut self.sidebar.tracker,
            ),
            None => SidebarModel::default(),
        };
        let next = model.next_change_at;
        self.sidebar.model = Arc::new(model);
        self.sidebar.wake_timer = next.map(|at| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis((at - now).max(0) as u64 + 1)).await;
                let _ = this.update(cx, |state, cx| {
                    state.rebuild_sidebar(cx);
                    cx.emit(AppEvent::Shell);
                });
            })
        });
        self.track_visit(cx);
    }

    pub fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        self.sidebar.shelves.toggle(shelf);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub fn show_more_settled(&mut self, cx: &mut Context<Self>) {
        self.sidebar.shelves.show_more_settled();
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Shell);
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
        cx.emit(AppEvent::Selection);
    }

    fn track_visit(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_thread.clone() else {
            self.sidebar.visit = None;
            return;
        };
        let Some(thread) = self.shell.thread(&id) else { return };
        let Some(updated_at) = t3_core::sidebar::time::parse_iso(&thread.updated_at) else { return };
        let now = now_ms();
        let shell = VisitedShell {
            updated_at,
            last_visited_at: thread.last_visited_at.as_deref().and_then(t3_core::sidebar::time::parse_iso),
            completed_at: self.sidebar.model.thread(&id).and_then(|t| t.latest_run.as_ref()).and_then(|run| run.completed_at),
            woke_at: self.sidebar.model.thread(&id).and_then(|t| settled::woke_at(t, now)),
        };
        if self.sidebar.visit.as_ref().is_none_or(|tracker| tracker.thread_id() != id) {
            self.sidebar.visit = Some(VisitTracker::new(&id));
        }
        let Some(tracker) = self.sidebar.visit.as_mut() else { return };
        match tracker.on_shell(shell, now) {
            VisitDecision::Skip => {}
            VisitDecision::Now { visited_at, opened } => self.send_visit(&id, visited_at, opened, cx),
            VisitDecision::At { due, visited_at } => {
                self.sidebar.visit_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis((due - now).max(0) as u64)).await;
                    let _ = this.update(cx, |state, cx| {
                        let Some(tracker) = state.sidebar.visit.as_mut().filter(|tracker| tracker.thread_id() == id) else { return };
                        if let VisitDecision::Now { visited_at, opened } = tracker.fire(visited_at, now_ms()) {
                            state.send_visit(&id, visited_at, opened, cx);
                        }
                    });
                }));
            }
        }
    }

    fn send_visit(&mut self, thread_id: &str, visited_at: Ms, opened: bool, cx: &mut Context<Self>) {
        let mut command = actions::visit(&uuid(), thread_id, visited_at);
        if opened {
            command["opened"] = Value::Bool(true);
        }
        self.dispatch_then(command, None, cx, |_, _, _| {});
    }

    /// Dispatches a command; on failure shows `failure_title` (when given) as an error toast.
    pub(crate) fn dispatch_then(
        &mut self,
        command: Value,
        failure_title: Option<&'static str>,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut AppState, bool, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client_handle() else { return };
        let task = self.runtime().spawn(async move { client.dispatch(command).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or(Err(RpcError::Closed));
            let _ = this.update(cx, |state, cx| {
                if let (Err(error), Some(title)) = (&result, failure_title) {
                    state.show_toast(Toast::new(ToastKind::Error, title).description(error.to_string()), cx);
                }
                then(state, result.is_ok(), cx);
            });
        })
        .detach();
    }

    pub fn show_toast(&mut self, toast: Toast, cx: &mut Context<Self>) {
        self.toast = Some(toast);
        cx.emit(AppEvent::Notice);
        self.toast_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |state, cx| {
                state.toast = None;
                cx.emit(AppEvent::Notice);
            });
        }));
    }

    fn after_undo_change(&mut self, cx: &mut Context<Self>) {
        cx.emit(AppEvent::Notice);
        let Some(at) = self.sidebar.undo.expires_at() else {
            self.sidebar.undo_timer = None;
            return;
        };
        let wait = (at - now_ms()).max(0) as u64;
        self.sidebar.undo_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(wait)).await;
            let _ = this.update(cx, |state, cx| {
                if state.sidebar.undo.expire(now_ms()) {
                    cx.emit(AppEvent::Notice);
                }
            });
        }));
    }

    fn snapshot_thread(&self, thread_id: &str) -> Option<t3_core::sidebar::thread::SidebarThread> {
        self.sidebar.model.thread(thread_id).cloned()
    }

    pub fn settle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        let now = now_ms();
        let woke = settled::woke_at(&thread, now);
        let plan = actions::plan_undo(PlanFor::Settle, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until);
        self.sidebar.undo.invalidate(UndoKind::Pin, thread_id);
        self.sidebar.undo.invalidate(UndoKind::Snooze, thread_id);
        let claim = self.sidebar.undo.begin(UndoKind::Settle, thread_id);
        let id = thread_id.to_owned();
        self.dispatch_then(actions::settle(&uuid(), thread_id), Some("Failed to settle thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            if let Some(woke) = woke {
                state.send_visit(&id, woke, false, cx);
            }
            state.sidebar.undo.show(UndoAction::Settled, claim, plan, now_ms());
            state.after_undo_change(cx);
        });
    }

    pub fn unsettle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.sidebar.undo.invalidate(UndoKind::Settle, thread_id);
        self.dispatch_then(actions::unsettle(&uuid(), thread_id), Some("Failed to un-settle thread"), cx, |_, _, _| {});
    }

    pub fn pin_thread(&mut self, thread_id: &str, order_key: Option<String>, cx: &mut Context<Self>) {
        let command = self.pin_command(thread_id, order_key);
        self.dispatch_then(command, Some("Failed to pin thread"), cx, |_, _, _| {});
    }

    /// `pinThread`: tops the pinned run by default, and supersedes older pin, settle and snooze
    /// undos, since pinning clears snooze and un-settles.
    fn pin_command(&mut self, thread_id: &str, order_key: Option<String>) -> Value {
        let order_key = order_key.or_else(|| top_of_pinned_order_key(self.sidebar.model.threads.iter()));
        for kind in [UndoKind::Pin, UndoKind::Settle, UndoKind::Snooze] {
            self.sidebar.undo.invalidate(kind, thread_id);
        }
        actions::pin(&uuid(), thread_id, order_key.as_deref())
    }

    pub fn unpin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        let plan = actions::plan_undo(PlanFor::Unpin, thread.pinned, thread.pin_order_key.as_deref(), thread.snoozed_until);
        let claim = self.sidebar.undo.begin(UndoKind::Pin, thread_id);
        self.dispatch_then(actions::unpin(&uuid(), thread_id), Some("Failed to unpin thread"), cx, move |state, ok, cx| {
            if ok && state.sidebar.undo.is_current(&claim) {
                state.sidebar.undo.show(UndoAction::Unpinned, claim, plan, now_ms());
                state.after_undo_change(cx);
            } else {
                state.sidebar.undo.finish(&claim);
            }
        });
    }

    pub fn snooze_thread(&mut self, thread_id: &str, snoozed_until: Ms, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        if !settled::can_snooze(&thread, now_ms()) {
            self.show_toast(
                Toast::new(ToastKind::Error, "Failed to snooze thread")
                    .description("This thread is waiting on you. Respond to the pending request before snoozing it."),
                cx,
            );
            return;
        }
        let claim = self.sidebar.undo.begin(UndoKind::Snooze, thread_id);
        self.dispatch_then(actions::snooze(&uuid(), thread_id, snoozed_until), Some("Failed to snooze thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            state.sidebar.undo.show(UndoAction::Snoozed, claim, actions::plan_undo(PlanFor::Snooze, false, None, None), now_ms());
            state.after_undo_change(cx);
        });
    }

    pub fn unsnooze_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.sidebar.undo.invalidate(UndoKind::Snooze, thread_id);
        self.dispatch_then(actions::unsnooze(&uuid(), thread_id), Some("Failed to wake thread"), cx, |_, _, _| {});
    }

    pub fn archive_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        if !thread.can_archive() {
            self.show_toast(Toast::new(ToastKind::Error, "Failed to archive thread").description("Stop the thread before archiving it."), cx);
            return;
        }
        let claim = self.sidebar.undo.begin(UndoKind::Archive, thread_id);
        let id = thread_id.to_owned();
        self.dispatch_then(actions::archive(&uuid(), thread_id), Some("Failed to archive thread"), cx, move |state, ok, cx| {
            if !ok {
                state.sidebar.undo.finish(&claim);
                return;
            }
            state.sidebar.undo.expire_placement(&id, false);
            state.sidebar.undo.show(UndoAction::Archived, claim, vec![UndoStep::Unarchive], now_ms());
            state.after_undo_change(cx);
            if state.selected_thread.as_deref() == Some(id.as_str()) {
                state.select_adjacent_after_removal(&id, cx);
            }
        });
    }

    /// Deletes after the caller confirmed. Stops a live runtime first, as the web does.
    pub fn delete_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        if thread.runtime.is_some() {
            self.dispatch_then(t3_core::commands::stop_thread(&uuid(), thread_id), None, cx, |_, _, _| {});
        }
        let id = thread_id.to_owned();
        let fallback = self.sidebar.model.adjacent_thread(Some(thread_id), true).or_else(|| self.sidebar.model.adjacent_thread(Some(thread_id), false)).map(str::to_owned);
        self.dispatch_then(actions::delete(&uuid(), thread_id), Some("Failed to delete thread"), cx, move |state, ok, cx| {
            if !ok {
                return;
            }
            state.sidebar.undo.expire_placement(&id, true);
            cx.emit(AppEvent::Notice);
            if state.selected_thread.as_deref() == Some(id.as_str()) {
                match fallback {
                    Some(next) => state.select_thread(&next, cx),
                    None => state.clear_selection(cx),
                }
            }
        });
    }

    fn select_adjacent_after_removal(&mut self, removed: &str, cx: &mut Context<Self>) {
        let next = self.sidebar.model.adjacent_thread(Some(removed), true).or_else(|| self.sidebar.model.adjacent_thread(Some(removed), false)).map(str::to_owned);
        match next {
            Some(next) => self.select_thread(&next, cx),
            None => self.clear_selection(cx),
        }
    }

    pub fn mark_thread_unread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.dispatch_then(actions::mark_unread(&uuid(), thread_id), None, cx, |_, _, _| {});
    }

    pub fn acknowledge_woke(&mut self, thread_id: &str, woke_at: Ms, cx: &mut Context<Self>) {
        self.send_visit(thread_id, woke_at, false, cx);
    }

    pub fn rename_thread(&mut self, thread_id: &str, title: &str, cx: &mut Context<Self>) {
        let Some(original) = self.snapshot_thread(thread_id).map(|thread| thread.title) else { return };
        match actions::resolve_rename(title, &original) {
            RenameOutcome::Empty => self.show_toast(Toast::new(ToastKind::Warning, "Thread title cannot be empty"), cx),
            RenameOutcome::Unchanged => {}
            RenameOutcome::Rename(title) => {
                self.dispatch_then(actions::rename(&uuid(), thread_id, &title), Some("Failed to rename thread"), cx, |_, _, _| {});
            }
        }
    }

    pub fn snooze_presets(&self) -> Vec<SnoozePreset> {
        settled::resolve_snooze_presets(&DEFAULT_SNOOZE_PRESETS, now_ms(), &SystemZone, &time_label)
    }

    /// Runs one context-menu or palette action for a thread.
    pub fn run_thread_action(&mut self, thread_id: &str, action: ThreadAction, shown_presets: &[SnoozePreset], cx: &mut Context<Self>) {
        let Some(thread) = self.snapshot_thread(thread_id) else { return };
        match action {
            ThreadAction::Settle => self.settle_thread(thread_id, cx),
            ThreadAction::Unsettle => self.unsettle_thread(thread_id, cx),
            ThreadAction::Pin => self.pin_thread(thread_id, None, cx),
            ThreadAction::Unpin => self.unpin_thread(thread_id, cx),
            ThreadAction::Unsnooze => self.unsnooze_thread(thread_id, cx),
            ThreadAction::SnoozePreset(id) => {
                if let Some(preset) = settled::pick_snooze_preset(shown_presets, &id, now_ms(), &DEFAULT_SNOOZE_PRESETS, &SystemZone, &time_label) {
                    self.snooze_thread(thread_id, preset.snoozed_until, cx);
                }
            }
            ThreadAction::AutoSettleEnabled | ThreadAction::AutoSettleDisabled => {
                let enabled = action == ThreadAction::AutoSettleEnabled;
                self.dispatch_then(actions::set_auto_settle(&uuid(), thread_id, enabled), Some("Failed to update auto-settle"), cx, |_, _, _| {});
            }
            ThreadAction::RegenerateTitle if !thread.regenerating_title => {
                self.dispatch_then(actions::regenerate_title(&uuid(), thread_id), Some("Failed to regenerate thread title"), cx, |_, _, _| {});
            }
            ThreadAction::MarkUnread => self.mark_thread_unread(thread_id, cx),
            ThreadAction::Archive => self.archive_thread(thread_id, cx),
            ThreadAction::CopyPath => {
                let path = thread.worktree_path.clone().or_else(|| self.shell.project(&thread.project_id).map(|project| project.workspace_root.clone()));
                match path {
                    Some(path) => self.copy(path.clone(), "Path copied", Some(path), cx),
                    None => self.show_toast(Toast::new(ToastKind::Error, "Path unavailable").description("This thread does not have a workspace path to copy."), cx),
                }
            }
            ThreadAction::CopyBranch => {
                if let Some(branch) = thread.branch.clone() {
                    self.copy(branch.clone(), "Branch copied", Some(branch), cx);
                }
            }
            ThreadAction::CopyThread => self.copy(actions::thread_reference(&thread.title, &thread.id), "Thread copied", Some(thread.title.clone()), cx),
            ThreadAction::CopyThreadId => self.copy(thread.id.clone(), "Thread ID copied", Some(thread.id.clone()), cx),
            _ => {}
        }
    }

    fn copy(&mut self, text: String, title: &'static str, description: Option<String>, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let toast = Toast::new(ToastKind::Success, title);
        self.show_toast(if let Some(description) = description { toast.description(description) } else { toast }, cx);
    }

    /// `undoLatestThreadAction`: restores the latest group of same-kind actions.
    pub fn undo_latest(&mut self, cx: &mut Context<Self>) -> bool {
        let group = self.sidebar.undo.take_latest_group();
        if group.is_empty() {
            return false;
        }
        self.after_undo_change(cx);
        for (thread_id, plan) in group {
            self.run_undo_steps(thread_id, plan, cx);
        }
        true
    }

    fn run_undo_steps(&mut self, thread_id: String, mut steps: Vec<UndoStep>, cx: &mut Context<Self>) {
        let now = now_ms();
        steps.retain(|step| actions::runnable_step(step, now));
        if steps.is_empty() {
            return;
        }
        let step = steps.remove(0);
        // Each step goes through its action's claim invalidation, as `runUndoPlan` calls the actions.
        let command = match &step {
            UndoStep::Unsettle => {
                self.sidebar.undo.invalidate(UndoKind::Settle, &thread_id);
                actions::unsettle(&uuid(), &thread_id)
            }
            UndoStep::Unsnooze => {
                self.sidebar.undo.invalidate(UndoKind::Snooze, &thread_id);
                actions::unsnooze(&uuid(), &thread_id)
            }
            UndoStep::Pin { order_key } => self.pin_command(&thread_id, order_key.clone()),
            UndoStep::Snooze { snoozed_until } => actions::snooze(&uuid(), &thread_id, *snoozed_until),
            UndoStep::Unarchive => {
                self.sidebar.undo.invalidate(UndoKind::Archive, &thread_id);
                actions::unarchive(&uuid(), &thread_id)
            }
        };
        let title = match step {
            UndoStep::Unarchive => "Failed to undo archive",
            UndoStep::Pin { .. } => "Failed to undo unpin",
            UndoStep::Unsnooze => "Failed to wake thread",
            _ => "Failed to undo settle",
        };
        self.dispatch_then(command, Some(title), cx, move |state, ok, cx| {
            if ok {
                state.run_undo_steps(thread_id, steps, cx);
            }
        });
    }
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
