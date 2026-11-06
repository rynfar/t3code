//! Multi-select and dragging on `AppState`: `handleThreadClick`, `handleMultiSelectContextMenu`
//! and `handleThreadDragEnd` in `apps/web/src/components/Sidebar.tsx`. The selection and drop
//! logic itself lives in `t3_core::sidebar::{selection, drag}`.

use std::collections::{HashMap, HashSet};

use gpui::{Context, Modifiers};
use t3_core::sidebar::Section;
use t3_core::sidebar::drag::{DropPlan, DropPlanInput, DropTarget, plan_thread_drop, position_of, resolve_drop_target_at_slot};
use t3_core::sidebar::selection::{BulkAction, BulkSelection};
use t3_core::sidebar::settled::{self, SnoozePreset};
use t3_core::thread_actions::{self as actions, UndoKind};

use super::controller::{Deletion, SystemZone, now_ms, time_label, uuid};
use crate::app_state::{AppEvent, AppState};

impl AppState {
    pub(crate) fn click_scoped_sidebar_thread(&mut self, key: &t3_core::sidebar::tag_filter::ThreadKey, modifiers: Modifiers, cx: &mut Context<Self>) {
        let toggle = if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control };
        if toggle {
            if !self.sidebar.fleet_selection.remove(key) { self.sidebar.fleet_selection.insert(key.clone()); }
        } else if modifiers.shift {
            let ordered: Vec<_> = self.sidebar.fleet.ordered.iter().map(|index| self.sidebar.fleet.threads[*index].key.clone()).collect();
            if let (Some(start), Some(end)) = (self.sidebar.fleet_anchor.as_ref().and_then(|anchor| ordered.iter().position(|key| key == anchor)), ordered.iter().position(|candidate| candidate == key)) {
                self.sidebar.fleet_selection.extend(ordered[start.min(end)..=start.max(end)].iter().cloned());
            }
        } else {
            self.sidebar.fleet_selection.clear();
            self.sidebar.fleet_anchor = Some(key.clone());
            if let Some(row) = self.scoped_thread(key) {
                self.navigate_scoped(row.owner.clone(), crate::app_state::ScopedRouteAction::Thread(key.thread_id.clone()), cx);
            }
        }
        self.selection_changed(cx);
    }

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
        let had = !self.sidebar.selection.is_empty() || !self.sidebar.fleet_selection.is_empty();
        let scoped = !self.sidebar.fleet_selection.is_empty(); self.sidebar.fleet_selection.clear();
        if self.sidebar.selection.clear() || scoped {
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
                let pinned = threads.iter().filter(|thread| thread.pinned).map(|thread| thread.id.clone()).collect();
                self.request_unpin_threads(pinned, cx);
            }
            BulkAction::Settle => {
                for thread in threads.iter().filter(|thread| !thread.is_settled()) {
                    self.settle_thread(&thread.id, cx);
                }
                self.clear_thread_selection(cx);
            }
            BulkAction::SnoozePreset(preset) => {
                let Some(preset) = settled::pick_snooze_preset(shown_presets, &preset, now_ms(), &self.snooze_settings(), &SystemZone, &time_label) else { return };
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

    /// `deleteSelectedThreadEntries`: failures stay selected; no co-deleted thread is a fallback.
    pub fn delete_threads(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let doomed = std::sync::Arc::new(ids.iter().cloned().collect());
        let target = self.delete_target();
        self.delete_next(ids, Deletion { doomed, target }, cx);
    }

    fn delete_next(&mut self, mut ids: Vec<String>, deletion: Deletion, cx: &mut Context<Self>) {
        if ids.is_empty() {
            let (shell, mounts) = (&self.shell, &self.omitted.mounts);
            let requested: Vec<_> = deletion.doomed.iter().cloned().collect();
            let missing = t3_core::sidebar::selection::keys_to_deselect_after_delete(
                &requested,
                &HashSet::new(),
                |id| mounts.thread(shell, id).is_some(),
            );
            if self.sidebar.selection.remove(&missing) {
                self.selection_changed(cx);
            }
            return;
        }
        let id = ids.remove(0);
        let key = id.clone();
        self.delete_thread_then(
            &key,
            deletion.clone(),
            "Failed to delete threads",
            cx,
            move |state, ok, cx| {
                if ok && state.sidebar.selection.remove(std::slice::from_ref(&id)) {
                    state.selection_changed(cx);
                }
                state.delete_next(ids, deletion, cx);
            },
        );
    }

    /// A row started dragging: the list shows its drop markers until the drag ends.
    pub fn start_thread_drag(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(from) = self.sidebar.model.section_of(thread_id).filter(|section| *section != Section::Working) else { return };
        self.sidebar.drag = Some((thread_id.to_owned(), from));
        self.rebuild_sidebar(cx);
        self.selection_changed(cx);
    }

    pub fn end_thread_drag(&mut self, cx: &mut Context<Self>) {
        let scoped = self.sidebar.fleet_drag.take().is_some();
        if self.sidebar.drag.take().is_some() || scoped {
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

impl AppState {
    pub fn start_scoped_thread_drag(&mut self, key: &t3_core::sidebar::tag_filter::ThreadKey, cx: &mut Context<Self>) {
        let Some(from) = self.sidebar.fleet.section_of(key).filter(|from| *from != Section::Working) else { return };
        self.sidebar.fleet_drag = Some((key.clone(), from)); self.rebuild_sidebar(cx); cx.emit(AppEvent::Selection);
    }

    fn scoped_drag_id(key: &t3_core::sidebar::tag_filter::ThreadKey) -> String { serde_json::to_string(&(&key.environment_id, &key.thread_id)).unwrap() }

    pub fn scoped_thread_drop_target(&self, slot: usize, cx: &gpui::App) -> Option<(DropTarget, DropPlan)> {
        use t3_core::sidebar::fleet::ScopedSidebarRow;
        use t3_core::sidebar::drag::{DropItem, Marker};
        let (key, from) = self.sidebar.fleet_drag.as_ref()?;
        let model = &self.sidebar.fleet;
        let mut items = Vec::new(); let mut before = 0;
        for (row_index, row) in model.rows.iter().enumerate() {
            let item = match row {
                ScopedSidebarRow::Thread { index, section } => Some(DropItem::Thread { id: Self::scoped_drag_id(&model.threads[*index].key), section: *section }),
                ScopedSidebarRow::Marker(marker) => Some(DropItem::Marker(*marker)),
                ScopedSidebarRow::Shelf { shelf, .. } => Some(DropItem::Marker(match shelf { t3_core::sidebar::Shelf::Working => Marker::WorkingHeader, t3_core::sidebar::Shelf::Snoozed => Marker::SnoozedHeader, t3_core::sidebar::Shelf::Settled => Marker::SettledHeader })),
                _ => None,
            };
            if let Some(item) = item { if row_index < slot { before += 1; } items.push(item); }
        }
        let id = Self::scoped_drag_id(key);
        let target = resolve_drop_target_at_slot(&items, position_of(&items, &id)?, before)?;
        let row = model.thread(key)?;
        let ids = |list: &[usize]| list.iter().map(|index| Self::scoped_drag_id(&model.threads[*index].key)).collect::<Vec<_>>();
        let pinned = ids(&model.partition.pinned); let active = ids(&model.partition.active);
        let pinned_keys = model.threads.iter().map(|row| (Self::scoped_drag_id(&row.key), row.thread.pin_order_key.clone())).collect();
        let active_keys = model.threads.iter().map(|row| (Self::scoped_drag_id(&row.key), row.thread.active_order_key.clone())).collect();
        let writable = model.threads.iter().filter(|row| self.scoped_ready(&row.owner, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).map(|row| Self::scoped_drag_id(&row.key)).collect();
        let plan = plan_thread_drop(&DropPlanInput { active_key: &id, active_section: *from, active_pinned: row.thread.pinned, active_settled: row.thread.is_settled(), supports_settlement: true,
            target: &target, pinned_order: &pinned, pinned_keys_by_id: &pinned_keys, reorderable_keys: Some(&writable), active_order: &active, active_keys_by_id: &active_keys, active_reorderable_keys: Some(&writable), active_time_ordered: self.sidebar.settings.working_shelf_enabled || crate::settings::store::client(cx).boring_sidebar_enabled() });
        (plan != DropPlan::None).then_some((target, plan))
    }

    pub fn drop_scoped_thread(&mut self, slot: usize, cx: &mut Context<Self>) {
        let planned = self.sidebar.fleet_drag.as_ref().map(|(key, _)| key.clone()).zip(self.scoped_thread_drop_target(slot, cx));
        self.end_thread_drag(cx);
        let Some((key, (_, plan))) = planned else { return };
        let id = Self::scoped_drag_id(&key);
        let mut writes = Vec::new();
        match plan {
            DropPlan::None => return,
            DropPlan::Settle => writes.push((id.clone(), actions::settle(&uuid(), &key.thread_id))),
            DropPlan::Pin { order_key, extra_assignments, .. } => {
                writes.push((id.clone(), actions::pin(&uuid(), &key.thread_id, order_key.as_deref())));
                writes.extend(extra_assignments.into_iter().map(|a| { let command = actions::reorder_pin(&uuid(), "", &a.order_key); (a.id, command) }));
            }
            DropPlan::ReorderPinned { assignments, .. } => writes.extend(assignments.into_iter().map(|a| { let command = actions::reorder_pin(&uuid(), "", &a.order_key); (a.id, command) })),
            DropPlan::MoveActive { assignments, unpin, unsettle, unsnooze, .. } => {
                if unpin { writes.push((id.clone(), actions::unpin(&uuid(), &key.thread_id))); }
                if unsettle { writes.push((id.clone(), actions::unsettle(&uuid(), &key.thread_id))); }
                if unsnooze { writes.push((id, actions::unsnooze(&uuid(), &key.thread_id))); }
                writes.extend(assignments.into_iter().map(|a| { let command = actions::reorder_active(&uuid(), "", &a.order_key); (a.id, command) }));
            }
        }
        let mut steps = Vec::new();
        for (id, mut command) in writes {
            let Ok((environment, thread)) = serde_json::from_str::<(String, String)>(&id) else { return };
            let key = t3_core::sidebar::tag_filter::ThreadKey::new(&environment, &thread);
            let Some(owner) = crate::context_menu::ThreadMenuOwner::capture_scoped(self, &key) else { return };
            if !owner.can_operate_owner(self) { return; }
            command["threadId"] = serde_json::json!(thread);
            steps.push((owner, command));
        }
        self.run_scoped_drop_steps(steps, cx);
    }

    pub(crate) fn run_scoped_drop_steps(&mut self, steps: Vec<(crate::context_menu::ThreadMenuOwner, serde_json::Value)>, cx: &mut Context<Self>) {
        let resolves = steps.iter().map(|(owner, _)| self.connections.destination(owner.destination.as_ref().unwrap(), t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)).collect::<Vec<_>>();
        let undo = steps.iter().map(|(owner, command)| self.scoped_undo_claim(owner, command.get("type").and_then(serde_json::Value::as_str).unwrap_or_default())).collect::<Vec<_>>();
        let stop = steps.iter().map(|(owner, command)| command.get("type").and_then(serde_json::Value::as_str) == Some("thread.delete") && owner.key().and_then(|key| self.scoped_thread(&key)).is_some_and(|row| row.thread.runtime.is_some())).collect::<Vec<_>>();
        cx.spawn(async move |this, cx| {
            let mut targets = Vec::new();
            for resolve in resolves { let Ok(target) = resolve.await else { return }; targets.push(target); }
            if !this.read_with(cx, |state, _| steps.iter().zip(&targets).all(|((owner, _), target)| target.is_current(&state.fleet) && owner.current(state) && owner.can_operate_owner(state)
                && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)))).unwrap_or(false) { return; }
            for (completed, ((((owner, command), target), undo), stop_first)) in steps.into_iter().zip(targets).zip(undo).zip(stop).enumerate() {
                if !this.read_with(cx, |state, _| target.is_current(&state.fleet) && owner.response_after_placement(state) && owner.can_operate_owner(state)
                    && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))).unwrap_or(false) {
                    let _ = this.update(cx, |state, cx| state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Thread action stopped")
                        .description(format!("{completed} change(s) saved before destination or permission changed")), cx));
                    return;
                }
                if stop_first {
                    if let Err(error) = target.client.dispatch(t3_core::commands::stop_thread(&uuid(), &owner.thread)).await {
                        let _ = this.update(cx, |state, cx| state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Thread action stopped")
                            .description(format!("{completed} change(s) saved before {}: {error}", owner.thread)), cx));
                        return;
                    }
                    if !this.read_with(cx, |state, _| target.is_current(&state.fleet) && owner.current(state) && owner.can_operate_owner(state)
                        && target.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))).unwrap_or(false) {
                        let _ = this.update(cx, |state, cx| state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Thread action stopped")
                            .description(format!("{completed} change(s) saved before destination or permission changed")), cx));
                        return;
                    }
                }
                if let Err(error) = target.client.dispatch(command).await {
                    let _ = this.update(cx, |state, cx| {
                        if let Some((claim, _, _)) = &undo { state.sidebar.undo.finish(claim); }
                        state.show_toast(crate::toast::Toast::new(crate::toast::ToastKind::Error, "Thread action stopped").description(format!("{completed} change(s) saved before {}: {error}", owner.thread)), cx);
                    }); return;
                }
                let _ = this.update(cx, |state, cx| {
                    if owner.response_after_placement(state) && owner.can_operate_owner(state) {
                        if let Some((claim, action, plan)) = undo {
                            state.sidebar.undo.show(action, claim, plan, now_ms()); state.after_undo_change(cx);
                            cx.emit(AppEvent::Notice);
                        }
                        if let Some(key) = owner.key() { state.sidebar.fleet_selection.remove(&key); }
                        cx.emit(AppEvent::SidebarSelection);
                    }
                });
            }
        }).detach();
    }
}

impl AppState {
    pub(crate) fn run_scoped_bulk_action(&mut self, owners: Vec<crate::context_menu::ThreadMenuOwner>, action: BulkAction, cx: &mut Context<Self>) {
        if owners.iter().any(|owner| !owner.current(self) || !owner.can_operate_owner(self)) { return; }
        let mut steps = Vec::new();
        for owner in owners {
            let Some(key) = owner.key() else { return };
            let Some(row) = self.scoped_thread(&key) else { return };
            let command = match action {
                BulkAction::Unpin if row.thread.pinned => actions::unpin(&uuid(), &key.thread_id),
                BulkAction::Settle if !row.thread.is_settled() => actions::settle(&uuid(), &key.thread_id),
                BulkAction::RegenerateTitles if !row.thread.regenerating_title => actions::regenerate_title(&uuid(), &key.thread_id),
                BulkAction::MarkUnread => actions::mark_unread(&uuid(), &key.thread_id),
                BulkAction::Delete => actions::delete(&uuid(), &key.thread_id),
                _ => continue,
            };
            steps.push((owner, command));
        }
        self.run_scoped_drop_steps(steps, cx);
    }
}
