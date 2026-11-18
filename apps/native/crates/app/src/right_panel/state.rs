//! Panel and diff-selection state on `AppState`, and the actions behind every entry point that
//! opens, closes or arranges the right panel (`components/chat/useRightPanelActions.ts`).
//! Persisted like the web's `localStorage` stores, written off the UI thread.

use std::collections::HashMap;

use gpui::{App, Context};
use serde_json::json;
use t3_client::store;
use t3_core::diff::{DiffSelection, TurnDiffSummary};
use t3_core::preview::{self, SessionSnapshot};
use t3_core::right_panel::{BrowserSnapshot, ClosedTarget, ClosedView, Reopened, RightPanels, Surface, SurfaceKind, decode_lenient_map};

use crate::app_state::{AppEvent, AppState};

/// The diff layout and file tree, which every environment shares.
const STORE: &str = "panels";

/// Thread-keyed panel state is per environment, as the web keys it by `scopedThreadKey`: a tab
/// saved for one server's thread never acts on another's.
fn environment_store(environment: &str) -> String {
    format!("panels-{environment}")
}

#[derive(Debug, Clone, Default)]
pub struct PanelState {
    pub panels: RightPanels,
    /// `t3code:diff-panel-state:v1` `byThreadKey`.
    pub diff_selections: HashMap<String, DiffSelection>,
    /// `branchBaseRefByThreadKey`.
    pub branch_base_refs: HashMap<String, String>,
    /// `t3code.diffFileTreeOpen`.
    pub diff_file_tree_open: bool,
    /// The thread whose diff was opened by an explicit request (a turn's "files changed"), so
    /// opening it keeps that selection instead of resetting to the branch scope.
    explicit_diff_open: Option<String>,
    /// The saved environment these threads belong to.
    environment: Option<String>,
}

impl PanelState {
    /// Each key decodes on its own: a corrupt entry falls back to its default and the rest load.
    pub fn load(environment: Option<&str>) -> PanelState {
        let shared = store::load_ui_state(STORE);
        let scoped = environment.and_then(|environment| store::load_ui_state(&environment_store(environment)));
        PanelState {
            panels: scoped.as_ref().and_then(|value| value.get("panels")).map(RightPanels::from_value).unwrap_or_default(),
            diff_selections: decode_lenient_map(scoped.as_ref().and_then(|value| value.get("diffSelections"))),
            branch_base_refs: decode_lenient_map(scoped.as_ref().and_then(|value| value.get("branchBaseRefs"))),
            diff_file_tree_open: shared.as_ref().and_then(|value| value.get("diffFileTreeOpen")).and_then(serde_json::Value::as_bool).unwrap_or(false),
            explicit_diff_open: None,
            environment: environment.map(str::to_owned),
        }
    }

    fn save(&self) {
        store::save_ui_state(STORE, &json!({ "diffFileTreeOpen": self.diff_file_tree_open }));
        if let Some(environment) = &self.environment {
            let scoped = json!({ "panels": self.panels, "diffSelections": self.diff_selections, "branchBaseRefs": self.branch_base_refs });
            store::save_ui_state(&environment_store(environment), &scoped);
        }
    }

    pub fn diff_selection(&self, thread_id: &str) -> DiffSelection {
        self.diff_selections.get(thread_id).cloned().unwrap_or_default()
    }

    fn diff_visible(&self, thread_id: &str) -> bool {
        self.panels.active_surface(thread_id).is_some_and(|surface| surface.kind == SurfaceKind::Diff)
    }
}

impl AppState {
    pub(crate) fn panel_thread(&self) -> Option<String> {
        self.selected_thread
            .clone()
            .or_else(|| self.draft_project.as_deref().map(t3_core::composer::drafts::new_thread_key))
    }

    /// Emits and persists after a change. When the diff became visible without an explicit
    /// request it shows the branch scope, as `ChatView.tsx` resets it on open.
    fn finish_panel_change(&mut self, thread_id: &str, diff_was_visible: bool, cx: &mut Context<Self>) {
        let explicit = self.panels.explicit_diff_open.take().as_deref() == Some(thread_id);
        if !diff_was_visible && self.panels.diff_visible(thread_id) && !explicit {
            self.reset_diff_to_branch(thread_id);
        }
        self.save_panels(cx);
    }

    fn reset_diff_to_branch(&mut self, thread_id: &str) {
        let previous = self.panels.diff_selection(thread_id);
        let remembered = self.panels.branch_base_refs.get(thread_id).map(String::as_str);
        let next = DiffSelection::git(&previous, remembered, false);
        self.panels.diff_selections.insert(thread_id.to_owned(), next);
    }

    pub(crate) fn save_panels(&mut self, cx: &mut Context<Self>) {
        self.sync_side_threads(cx);
        cx.emit(AppEvent::RightPanel);
        self.panels.save();
    }

    pub(crate) fn mutate_panels(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PanelState, &str) -> bool) {
        let Some(thread_id) = self.panel_thread() else { return };
        let diff_was_visible = self.panels.diff_visible(&thread_id);
        let before = self.panels.panels.panel(&thread_id).surfaces.clone();
        if change(&mut self.panels, &thread_id) {
            let after = &self.panels.panels.panel(&thread_id).surfaces;
            let closed: Vec<Surface> = before.into_iter().filter(|surface| !after.contains(surface)).collect();
            self.discard_closed_side_chats(&closed, cx);
            self.finish_panel_change(&thread_id, diff_was_visible, cx);
        } else {
            self.panels.explicit_diff_open = None;
        }
    }

    /// `rightPanel.toggle` (`cmd-alt-b`) and the header's panel toggle.
    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.mutate_panels(cx, |state, thread| {
            if state.panels.is_open(thread) {
                let unmaximized = state.panels.set_maximized(thread, false);
                state.panels.hide(thread) || unmaximized
            } else {
                state.panels.toggle_visibility(thread)
            }
        });
    }

    /// `diff.toggle` (`cmd-d`).
    pub fn toggle_diff(&mut self, cx: &mut Context<Self>) {
        if self.selected_thread.is_none() {
            return;
        }
        self.mutate_panels(cx, |state, thread| state.panels.toggle(thread, Surface::diff()));
    }

    /// The "Linked pull requests" launcher entry and "Show linked pull requests".
    pub fn add_pull_requests_surface(&mut self, cx: &mut Context<Self>) {
        if self.selected_thread.is_none() {
            return;
        }
        self.mutate_panels(cx, |state, thread| {
            state.panels.open(thread, Surface::pull_requests());
            true
        });
    }

    /// The "Diff" launcher entry: always opens on the branch scope (`addDiffSurface`).
    pub fn add_diff_surface(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        self.reset_diff_to_branch(&thread_id);
        self.panels.explicit_diff_open = Some(thread_id);
        self.mutate_panels(cx, |state, thread| {
            state.panels.open(thread, Surface::diff());
            true
        });
    }

    /// `onOpenTurnDiff`: a turn's changed-files summary opens the diff on that turn, revealing
    /// `file_path` when given. `run_id` is the checkpoint's run (`TurnDiffSummary::run_id`).
    pub fn open_turn_diff(&mut self, run_id: &str, file_path: Option<&str>, cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        let previous = self.panels.diff_selection(&thread_id);
        let next = DiffSelection::turn(&previous, run_id, file_path);
        self.panels.diff_selections.insert(thread_id.clone(), next);
        self.panels.explicit_diff_open = Some(thread_id);
        self.mutate_panels(cx, |state, thread| {
            state.panels.open(thread, Surface::diff());
            true
        });
    }

    /// `parentPanel.openToolDiff`: keep the panel owner selected, even for a side chat's tool.
    pub fn open_tool_call_diff(&mut self, target: t3_core::diff::ToolCallTarget, cx: &mut Context<Self>) {
        if !self.can_read_threads() || !target.source_available(self.thread_shell(&target.thread_id), self.thread_state(&target.thread_id)) {
            return;
        }
        let Some(thread_id) = self.panel_thread() else { return };
        // Select before replacing the side chat so its existing subscription is retained.
        self.panels.diff_selections.insert(thread_id.clone(), DiffSelection::ToolCall(target));
        self.panels.explicit_diff_open = Some(thread_id);
        self.mutate_panels(cx, |state, thread| {
            state.panels.open(thread, Surface::diff());
            true
        });
    }

    /// Clicking a tab. Activating the diff counts as an explicit open: it keeps its selection.
    pub fn activate_surface(&mut self, surface_id: &str, cx: &mut Context<Self>) {
        if let Some(thread_id) = self.panel_thread()
            && self.panels.panels.panel(&thread_id).surfaces.iter().any(|surface| surface.id == surface_id && surface.kind == SurfaceKind::Diff)
        {
            self.panels.explicit_diff_open = Some(thread_id);
        }
        self.mutate_panels(cx, |state, thread| state.panels.activate(thread, surface_id));
    }

    pub fn close_surface(&mut self, surface_id: &str, cx: &mut Context<Self>) {
        self.close_surfaces(cx, |panels, thread| panels.close_surface(thread, surface_id));
    }

    pub fn close_other_surfaces(&mut self, surface_id: &str, cx: &mut Context<Self>) {
        self.close_surfaces(cx, |panels, thread| panels.close_other_surfaces(thread, surface_id));
    }

    pub fn close_surfaces_to_right(&mut self, surface_id: &str, cx: &mut Context<Self>) {
        self.close_surfaces(cx, |panels, thread| panels.close_surfaces_to_right(thread, surface_id));
    }

    pub fn close_all_surfaces(&mut self, cx: &mut Context<Self>) {
        self.close_surfaces(cx, |panels, thread| panels.close_all_surfaces(thread));
    }

    /// A user's close. Browser tabs it removes are remembered with their session's snapshot
    /// (`closePreviewSession`), active tab last; a session the server already closed has none.
    fn close_surfaces(&mut self, cx: &mut Context<Self>, close: impl FnOnce(&mut RightPanels, &str) -> bool) {
        let Some(thread_id) = self.panel_thread() else { return };
        let browsers = self.browser_snapshots(&thread_id, cx);
        self.mutate_panels(cx, |state, thread| {
            if !close(&mut state.panels, thread) {
                return false;
            }
            let open = &state.panels.panel(thread).surfaces;
            let closed: Vec<BrowserSnapshot> = browsers.into_iter().filter(|(id, _)| !open.iter().any(|surface| surface.id == *id)).map(|(_, snapshot)| snapshot).collect();
            for snapshot in closed {
                state.panels.remember_browser(thread, snapshot);
            }
            true
        });
    }

    /// The thread's browser tabs and their sessions' snapshots, the active tab last.
    fn browser_snapshots(&self, thread_id: &str, cx: &App) -> Vec<(String, BrowserSnapshot)> {
        let Some(previews) = crate::preview::previews(cx) else { return Vec::new() };
        let previews = previews.read(cx);
        let panel = self.panels.panels.panel(thread_id);
        let mut snapshots: Vec<(String, BrowserSnapshot)> = panel
            .surfaces
            .iter()
            .filter_map(|surface| Some((surface.id.clone(), BrowserSnapshot::from_session(previews.session(thread_id, surface.preview_tab()?)?))))
            .collect();
        snapshots.sort_by_key(|(id, _)| panel.active_surface_id.as_deref() == Some(id.as_str()));
        snapshots
    }

    /// `rightPanel.close` (`cmd-w`): closes the active tab while the panel shows.
    pub fn close_active_surface(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        let Some(active) = self.panels.panels.active_surface(&thread_id).map(|surface| surface.id.clone()) else { return };
        self.close_surface(&active, cx);
    }

    pub fn toggle_right_panel_maximized(&mut self, cx: &mut Context<Self>) {
        self.mutate_panels(cx, |state, thread| {
            let maximized = state.panels.panel(thread).maximized;
            state.panels.is_open(thread) && state.panels.set_maximized(thread, !maximized)
        });
    }

    /// `view.reopenClosed` (`cmd-shift-t`): may switch to the thread the tab belonged to.
    pub fn reopen_closed_view(&mut self, cx: &mut Context<Self>) {
        let (shell, mounts) = (&self.shell, &self.omitted.mounts);
        let thread_exists = |thread: &str| mounts.thread(shell, thread).is_some();
        let workspace_available = |thread: &str| {
            mounts.thread(shell, thread).is_some_and(|thread| {
                thread.worktree_path.as_deref().is_some_and(|p| !p.is_empty())
                    || shell.project(&thread.project_id).is_some_and(|project| !project.workspace_root.is_empty())
            })
        };
        let before = self.panels.panels.closed_views().to_vec();
        let reopened = self.panels.panels.reopen_closed(thread_exists, shell.synchronized, workspace_available);
        match reopened {
            Some(Reopened::Thread(thread_id)) => {
                self.select_thread(&thread_id, cx);
                // A restored tab was not showing before, so a restored diff opens like any other open.
                if self.panels.diff_visible(&thread_id) {
                    self.reset_diff_to_branch(&thread_id);
                }
                self.save_panels(cx);
            }
            Some(Reopened::Browser(view)) => self.reopen_browser(view, cx),
            // Dropped entries, or a failed restore moving its entry to the back, still save.
            None if self.panels.panels.closed_views() != before.as_slice() => self.panels.save(),
            None => {}
        }
    }

    /// The browser arm of `reopenClosedView`: a new session with the closed one's page and
    /// settings, then its tab in front of its thread. A failure defers the entry.
    fn reopen_browser(&mut self, view: ClosedView, cx: &mut Context<Self>) {
        let ClosedTarget::Browser { snapshot } = &view.target else { return };
        let previews = crate::preview::previews(cx).filter(|previews| previews.read(cx).availability() == crate::preview::Availability::Available);
        let task = previews.as_ref().and_then(|_| self.request(preview::method::OPEN, snapshot.open_input(&view.thread_id), SessionSnapshot::decode));
        let (Some(previews), Some(task)) = (previews, task) else {
            self.panels.panels.defer_closed(view);
            self.save_panels(cx);
            return;
        };
        self.save_panels(cx);
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Disconnected.".to_owned()));
            let _ = this.update(cx, |state, cx| match result {
                Ok(opened) => {
                    state.select_thread(&view.thread_id, cx);
                    let thread = view.thread_id.clone();
                    state.mutate_panels(cx, |panels, current| current == thread && panels.panels.open_browser(current, Some(&opened.tab_id)));
                    // The preview controller fronts it again once its session arrives.
                    cx.defer(move |cx| previews.update(cx, |previews, cx| previews.popup(&thread, &opened.tab_id, cx)));
                }
                Err(error) => {
                    tracing::warn!(%error, "reopening a closed browser tab failed");
                    state.panels.panels.defer_closed(view);
                    state.save_panels(cx);
                }
            });
        })
        .detach();
    }

    /// Width while dragging the resize handle; `persist` once the drag settles.
    pub fn set_right_panel_width(&mut self, width: f32, persist: bool, cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        if self.panels.panels.set_width(&thread_id, width) || persist {
            if persist {
                self.save_panels(cx);
            } else {
                cx.emit(AppEvent::RightPanel);
            }
        }
    }

    /// A thread came into view: if its diff shows, it opens on the branch scope.
    pub fn right_panel_thread_shown(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        if self.panels.diff_visible(&thread_id) {
            self.reset_diff_to_branch(&thread_id);
            self.save_panels(cx);
        }
    }

    pub fn select_diff(&mut self, selection: DiffSelection, cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        if let DiffSelection::Branch { base_ref: Some(base_ref) } = &selection {
            self.panels.branch_base_refs.insert(thread_id.clone(), base_ref.clone());
        }
        if self.panels.diff_selections.get(&thread_id) != Some(&selection) {
            self.panels.diff_selections.insert(thread_id, selection);
            self.save_panels(cx);
        }
    }

    /// `reconcileTurnSelection`: a rolled-back turn falls back to the latest.
    pub fn reconcile_diff_selection(&mut self, ordered: &[TurnDiffSummary], cx: &mut Context<Self>) {
        let Some(thread_id) = self.panel_thread() else { return };
        let Some(selection) = self.panels.diff_selections.get_mut(&thread_id) else { return };
        if selection.reconcile(ordered) {
            self.save_panels(cx);
        }
    }

    pub fn set_diff_file_tree_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.panels.diff_file_tree_open != open {
            self.panels.diff_file_tree_open = open;
            self.save_panels(cx);
        }
    }
}
