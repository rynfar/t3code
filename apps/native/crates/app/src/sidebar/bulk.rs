//! Multi-select and dragging on `AppState`: `handleThreadClick`, `handleMultiSelectContextMenu`
//! and `handleThreadDragEnd` in `apps/web/src/components/Sidebar.tsx`. The selection and drop
//! logic itself lives in `t3_core::sidebar::{selection, drag}`.

use std::collections::{HashMap, HashSet};

use gpui::{Context, Modifiers};
use t3_core::sidebar::Section;
use t3_core::sidebar::drag::{DropPlan, DropPlanInput, DropTarget, plan_thread_drop, position_of, resolve_drop_target_at_slot};
use t3_core::sidebar::selection::{BulkAction, BulkSelection};
use t3_core::sidebar::settled::{self, DEFAULT_SNOOZE_PRESETS, SnoozePreset};
use t3_core::thread_actions::{self as actions, UndoKind};

use super::controller::{SystemZone, now_ms, time_label, uuid};
use crate::app_state::{AppEvent, AppState};

impl AppState {
    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(AppEvent::SidebarSelection);
    }

    /// `handleThreadClick`: the platform modifier toggles a row, Shift extends from the anchor,
    /// and a plain click opens the thread, clearing the selection and anchoring there.
    pub fn click_sidebar_thread(&mut self, thread_id: &str, modifiers: Modifiers, cx: &mut Context<Self>) {
        let toggle = if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control };
        if toggle {
            self.sidebar.selection.toggle(thread_id);
            return self.selection_changed(cx);
        }
        if modifiers.shift {
            let model = self.sidebar.model.clone();
            let ordered: Vec<&str> = model.ordered.iter().map(|index| model.threads[*index].id.as_str()).collect();
            self.sidebar.selection.range_select_to(thread_id, &ordered);
            return self.selection_changed(cx);
        }
        if self.sidebar.selection.clear() {
            self.selection_changed(cx);
        }
        self.sidebar.selection.set_anchor(thread_id);
        self.select_thread(thread_id, cx);
    }

    /// Escape and the bulk actions clear the selection. Returns whether there was one.
    pub fn clear_thread_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let had = !self.sidebar.selection.is_empty();
        if self.sidebar.selection.clear() {
            self.selection_changed(cx);
        }
        had
    }

    /// The selected threads still listed, and what the bulk menu offers for them.
    pub fn bulk_selection(&self) -> (Vec<String>, BulkSelection) {
        let model = &self.sidebar.model;
        let now = now_ms();
        let threads: Vec<_> = self.sidebar.selection.selected().iter().filter_map(|id| model.thread(id)).collect();
        let caps = self.sidebar.capabilities;
        let summary = BulkSelection {
            count: threads.len(),
            pinned: threads.iter().filter(|thread| thread.pinned).count(),
            settling: threads.iter().filter(|thread| !thread.is_settled()).count(),
            can_snooze: threads.iter().all(|thread| settled::can_snooze(thread, now)),
            regeneratable: threads.iter().filter(|thread| !thread.regenerating_title).count(),
            config_loaded: caps.config_loaded,
            can_operate: caps.can_operate,
        };
        (threads.iter().map(|thread| thread.id.clone()).collect(), summary)
    }

    /// Runs a bulk menu action. Delete and Custom snooze go through their dialogs first.
    pub fn run_bulk_action(&mut self, ids: &[String], action: BulkAction, shown_presets: &[SnoozePreset], cx: &mut Context<Self>) {
        let model = self.sidebar.model.clone();
        let threads: Vec<_> = ids.iter().filter_map(|id| model.thread(id)).collect();
        match action {
            BulkAction::Unpin => {
                for thread in threads.iter().filter(|thread| thread.pinned) {
                    self.unpin_thread(&thread.id, cx);
                }
                self.clear_thread_selection(cx);
            }
            BulkAction::Settle => {
                for thread in threads.iter().filter(|thread| !thread.is_settled()) {
                    self.settle_thread(&thread.id, cx);
                }
                self.clear_thread_selection(cx);
            }
            BulkAction::SnoozePreset(preset) => {
                let Some(preset) = settled::pick_snooze_preset(shown_presets, &preset, now_ms(), &DEFAULT_SNOOZE_PRESETS, &SystemZone, &time_label) else { return };
                self.clear_thread_selection(cx);
                for thread in &threads {
                    self.snooze_thread(&thread.id, preset.snoozed_until, cx);
                }
            }
            BulkAction::RegenerateTitles => {
                let ids = threads.iter().filter(|thread| !thread.regenerating_title).map(|thread| thread.id.clone()).collect();
                self.regenerate_titles(ids, cx);
            }
            BulkAction::MarkUnread => {
                for thread in &threads {
                    self.mark_thread_unread(&thread.id, cx);
                }
                self.clear_thread_selection(cx);
            }
            BulkAction::Delete => self.delete_threads(ids.to_vec(), cx),
            BulkAction::Snooze | BulkAction::SnoozeCustom => {}
        }
    }

    /// One at a time, stopping at the first failure, then clears the selection.
    fn regenerate_titles(&mut self, mut ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            self.clear_thread_selection(cx);
            return;
        }
        let id = ids.remove(0);
        self.dispatch_then(actions::regenerate_title(&uuid(), &id), Some("Failed to regenerate thread titles"), cx, move |state, ok, cx| {
            if ok {
                state.regenerate_titles(ids, cx);
            }
        });
    }

    /// `deleteSelectedThreadEntries`: deletes in order, keeps a failed thread selected, and opens
    /// the nearest surviving thread when the open one goes.
    pub fn delete_threads(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let doomed: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let model = self.sidebar.model.clone();
        let ordered: Vec<&str> = model.ordered.iter().map(|index| model.threads[*index].id.as_str()).collect();
        let fallback = self.selected_thread.as_deref().and_then(|open| {
            let at = ordered.iter().position(|id| *id == open)?;
            ordered[at..].iter().chain(ordered[..at].iter().rev()).find(|id| !doomed.contains(**id)).map(|id| (*id).to_owned())
        });
        self.delete_next(ids, fallback, cx);
    }

    fn delete_next(&mut self, mut ids: Vec<String>, fallback: Option<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            let (shell, mounts) = (&self.shell, &self.omitted.mounts);
            let missing: Vec<String> = self.sidebar.selection.selected().iter().filter(|id| mounts.thread(shell, id).is_none()).cloned().collect();
            if self.sidebar.selection.remove(&missing) {
                self.selection_changed(cx);
            }
            return;
        }
        let id = ids.remove(0);
        let next_fallback = fallback.clone();
        let key = id.clone();
        self.delete_thread_then(&key, fallback, "Failed to delete threads", cx, move |state, ok, cx| {
            if ok && state.sidebar.selection.remove(std::slice::from_ref(&id)) {
                state.selection_changed(cx);
            }
            state.delete_next(ids, next_fallback, cx);
        });
    }

    /// A row started dragging: the list shows its drop markers until the drag ends.
    pub fn start_thread_drag(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(from) = self.sidebar.model.section_of(thread_id).filter(|section| *section != Section::Working) else { return };
        self.sidebar.drag = Some((thread_id.to_owned(), from));
        self.rebuild_sidebar(cx);
        self.selection_changed(cx);
    }

    pub fn end_thread_drag(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.drag.take().is_some() {
            self.rebuild_sidebar(cx);
            self.selection_changed(cx);
        }
    }

    /// The drop target for the dragged thread at insertion `slot`, when the drop would change
    /// anything (`createSidebarCollisionDetection`'s validity check).
    pub fn thread_drop_target(&self, slot: usize) -> Option<(DropTarget, DropPlan)> {
        let (id, from) = self.sidebar.drag.as_ref()?;
        let items = self.sidebar.model.drop_items();
        let target = resolve_drop_target_at_slot(&items, position_of(&items, id)?, slot)?;
        let plan = self.plan_drop(id, *from, &target)?;
        Some((target, plan))
    }

    fn plan_drop(&self, id: &str, from: Section, target: &DropTarget) -> Option<DropPlan> {
        let model = &self.sidebar.model;
        let thread = model.thread(id)?;
        let snapshot = self.shell.snapshot.as_ref()?;
        let ids = |list: &[usize]| list.iter().map(|index| model.threads[*index].id.clone()).collect::<Vec<_>>();
        let pinned_order = ids(&model.partition.pinned);
        let active_order = ids(&model.partition.active);
        let pinned_keys: HashMap<String, Option<String>> = snapshot.threads.iter().map(|thread| (thread.id.clone(), thread.pin_order_key.clone())).collect();
        let active_keys: HashMap<String, Option<String>> = snapshot.threads.iter().map(|thread| (thread.id.clone(), thread.active_order_key.clone())).collect();
        // One environment: every row is writable while it is connected with a loaded config.
        let writable: HashSet<String> = if self.sidebar.capabilities.config_loaded { model.threads.iter().map(|thread| thread.id.clone()).collect() } else { HashSet::new() };
        let plan = plan_thread_drop(&DropPlanInput {
            active_key: id,
            active_section: from,
            active_pinned: thread.pinned,
            active_settled: thread.is_settled(),
            supports_settlement: self.sidebar.capabilities.config_loaded,
            target,
            pinned_order: &pinned_order,
            pinned_keys_by_id: &pinned_keys,
            reorderable_keys: Some(&writable),
            active_order: &active_order,
            active_keys_by_id: &active_keys,
            active_reorderable_keys: Some(&writable),
            active_time_ordered: self.sidebar.settings.working_shelf_enabled,
        });
        (plan != DropPlan::None).then_some(plan)
    }

    /// `handleThreadDragEnd`: runs the lifecycle change, then the order-key writes in sequence.
    pub fn drop_thread(&mut self, slot: usize, cx: &mut Context<Self>) {
        let plan = self.sidebar.drag.as_ref().map(|(id, _)| id.clone()).zip(self.thread_drop_target(slot));
        self.end_thread_drag(cx);
        let Some((id, (_, plan))) = plan else { return };
        if !self.sidebar.capabilities.can_operate {
            return;
        }
        match plan {
            DropPlan::None => {}
            DropPlan::Settle => self.settle_thread(&id, cx),
            DropPlan::MoveActive { assignments, unpin, unsettle, unsnooze, .. } => {
                let mut steps = Vec::new();
                for (applies, kind) in [(unpin, UndoKind::Pin), (unsettle, UndoKind::Settle), (unsnooze, UndoKind::Snooze)] {
                    if applies {
                        self.sidebar.undo.invalidate(kind, &id);
                    }
                }
                if unpin {
                    steps.push((actions::unpin(&uuid(), &id), "Failed to unpin thread"));
                }
                if unsettle {
                    steps.push((actions::unsettle(&uuid(), &id), "Failed to un-settle thread"));
                }
                if unsnooze {
                    steps.push((actions::unsnooze(&uuid(), &id), "Failed to wake thread"));
                }
                steps.extend(assignments.iter().map(|assignment| (actions::reorder_active(&uuid(), &assignment.id, &assignment.order_key), "Failed to reorder active threads")));
                self.run_drop_steps(steps, cx);
            }
            DropPlan::Pin { order_key, extra_assignments, .. } => {
                let mut steps = vec![(self.pin_command(&id, order_key), "Failed to pin thread")];
                steps.extend(extra_assignments.iter().map(|assignment| (actions::reorder_pin(&uuid(), &assignment.id, &assignment.order_key), "Failed to reorder pinned threads")));
                self.run_drop_steps(steps, cx);
            }
            DropPlan::ReorderPinned { assignments, .. } => {
                let steps = assignments.iter().map(|assignment| (actions::reorder_pin(&uuid(), &assignment.id, &assignment.order_key), "Failed to reorder pinned threads")).collect();
                self.run_drop_steps(steps, cx);
            }
        }
    }

    fn run_drop_steps(&mut self, mut steps: Vec<(serde_json::Value, &'static str)>, cx: &mut Context<Self>) {
        if steps.is_empty() {
            return;
        }
        let (command, failure) = steps.remove(0);
        self.dispatch_then(command, Some(failure), cx, move |state, ok, cx| {
            if ok {
                state.run_drop_steps(steps, cx);
            }
        });
    }
}
