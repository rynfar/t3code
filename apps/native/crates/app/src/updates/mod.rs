//! Sidebar update notices and confirmation from the web's `SidebarProviderUpdatePill.tsx`,
//! `SidebarUpdatePill.tsx`, `SidebarUpdateCapsule.tsx`, and `UpdateEverythingDialog.tsx`.
//! Indicators are static at idle.
mod dialog;
#[cfg(all(test, feature = "snapshot"))]
mod tests;
use crate::settings::controls::{activates, focus_handle};
use gpui::{Div, FontWeight, KeyBinding, KeyDownEvent, MouseButton, Stateful, actions, anchored, deferred};
actions!(updates, [FocusNextUpdate, FocusPreviousUpdate, CloseCapsulePopover]);
const DIALOG_CONTEXT: &str = "UpdateEverythingDialog";
const CONTROL_CONTEXT: &str = "SidebarUpdates";
pub fn init(cx: &mut App) {
    for context in [DIALOG_CONTEXT, CONTROL_CONTEXT] {
        cx.bind_keys([
            KeyBinding::new("tab", FocusNextUpdate, Some(context)),
            KeyBinding::new("shift-tab", FocusPreviousUpdate, Some(context)),
        ]);
    }
    cx.bind_keys([KeyBinding::new(
        "escape",
        CloseCapsulePopover,
        Some(CONTROL_CONTEXT),
    )]);
}

fn keyboard_button(
    element: Stateful<Div>,
    id: &'static str,
    enabled: bool,
    activate: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let activate = std::rc::Rc::new(activate);
    let on_key = activate.clone();
    let ring = hsla(palette(cx).ring);
    let focus = enabled.then(|| focus_handle(&id.into(), window, cx));
    element
        .when_some(focus, |element, focus| element.track_focus(&focus))
        .focus_visible(move |s| {
            s.shadow(vec![gpui::BoxShadow {
                color: ring,
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
                inset: true,
            }])
        })
        .on_click(move |event, window, cx| {
            if enabled && !event.is_keyboard() {
                activate(window, cx);
            }
        })
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if enabled && activates(event) {
                cx.stop_propagation();
                on_key(window, cx);
            }
        })
}

#[cfg(feature = "snapshot")]
mod fixture;

use crate::{
    app_state::{AppEvent, AppState, Phase},
    icons::Icon,
    theme::{hsla, palette, rem},
    toast::{Toast, ToastKind},
};
use gpui::{
    App, Context, Entity, Global, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use std::{collections::HashSet, time::Duration};
use t3_client::updates::{CapturedUpdateOwner, UpdateDestination, UpdateEvent};
use t3_core::{
    settings::search::Section,
    updates::{
        self, CapsuleAction, CapsuleTone, Machine, MachineResult, Pill, Plan,
        SAVED_RESULT_STORE_KEY, SavedUpdateEverythingResult, ServerTarget, ServerUpdateStage, Tone,
        UpdateEverythingResult, UpdateEverythingRun, UpdateRunMachineStatus, UpdateRunPhase,
    },
};

struct Host {
    control: WeakEntity<UpdateControl>,
    dialog: WeakEntity<dialog::UpdateDialog>,
}
impl Global for Host {}
impl gpui::EventEmitter<()> for UpdateControl {}
pub fn overlay(cx: &App) -> Option<Entity<dialog::UpdateDialog>> {
    cx.try_global::<Host>()?.dialog.upgrade()
}

struct ResumedHandoff {
    remaining: usize,
    updated_servers: usize,
    failures: Vec<String>,
}

pub struct UpdateControl {
    state: Entity<AppState>,
    dialog: Entity<dialog::UpdateDialog>,
    plan: Plan,
    pill: Option<Pill>,
    dismissed: HashSet<String>,
    visible_after: Option<String>,
    connection: Option<u64>,
    success_timer: Option<Task<()>>,
    run: Option<UpdateEverythingRun>,
    stages: Vec<(String, Option<ServerUpdateStage>)>,
    captures: Vec<CapturedUpdateOwner>,
    capsule_open: bool,
    capsule_timer: Option<Task<()>>,
    handoff_timer: Option<Task<()>>,
    active_runs: usize,
    run_updated_servers: Vec<String>,
    run_updated_providers: usize,
    run_provider_failures: Vec<String>,
    resumed_handoff: Option<ResumedHandoff>,
    #[cfg(test)]
    last_saved_result: Option<SavedUpdateEverythingResult>,
    fixture_now_ms: Option<i64>,
    fixture: bool,
    _subscriptions: Vec<Subscription>,
    _grants: Option<Task<()>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn stage_for(
    stages: &[(String, Option<ServerUpdateStage>)],
    environment_id: &str,
) -> Option<ServerUpdateStage> {
    stages
        .iter()
        .find(|(id, _)| id == environment_id)
        .and_then(|(_, stage)| *stage)
}

fn set_stage(
    stages: &mut Vec<(String, Option<ServerUpdateStage>)>,
    environment_id: &str,
    stage: Option<ServerUpdateStage>,
) {
    if let Some((_, existing)) = stages.iter_mut().find(|(id, _)| id == environment_id) {
        *existing = stage;
    } else if stage.is_some() {
        stages.push((environment_id.to_owned(), stage));
    }
    if stage.is_none() {
        stages.retain(|(_, s)| s.is_some());
    }
}

fn target_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        // Native's crate version is independent of the server release it was built alongside.
        serde_json::from_str::<serde_json::Value>(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../server/package.json"
        )))
        .ok()
        .and_then(|v| v["version"].as_str().map(str::to_owned))
        .unwrap_or_default()
    });
    &VERSION
}

fn connected(state: &AppState) -> bool {
    matches!(
        state.phase,
        Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)
    )
}
#[cfg(all(test, feature = "snapshot"))]
pub(super) fn plan(state: &AppState, updating: bool) -> Plan {
    plan_with(state, |_| updating)
}

pub(super) fn plan_with(state: &AppState, is_updating: impl FnMut(&str) -> bool) -> Plan {
    collect_with(state, is_updating, |machines| {
        updates::collect(machines, target_version())
    })
}

#[cfg(all(test, feature = "snapshot"))]
pub(super) fn channel_switch_plan_with(
    state: &AppState,
    release: &updates::ChannelSwitchRelease,
    is_updating: impl FnMut(&str) -> bool,
) -> updates::ChannelSwitchPlan {
    collect_with(state, is_updating, |machines| {
        updates::collect_channel_switch(machines, release)
    })
}

fn collect_with<T>(
    state: &AppState,
    mut is_updating: impl FnMut(&str) -> bool,
    collect: impl FnOnce(&[Machine<'_>]) -> T,
) -> T {
    let machines: Vec<_> = state
        .fleet
        .environments
        .iter()
        .map(|entry| {
            let owner = &entry.summary.owner;
            let saved_label = state
                .environments
                .environments
                .iter()
                .find(|saved| saved.id == owner.saved_id)
                .map(|saved| saved.label.as_str())
                .filter(|label| !label.trim().is_empty());
            let label = entry
                .config
                .config
                .as_ref()
                .map(|c| c.environment.label.as_str())
                .filter(|label| !label.trim().is_empty())
                .or(saved_label)
                .unwrap_or(owner.saved_id.as_str());
            Machine {
                id: &owner.saved_id,
                label,
                config: entry.config.config.as_ref(),
                connected: entry.status == t3_client::supervisor::ConnectionStatus::Connected
                    && entry.summary.synchronized
                    && entry.summary.config_loaded,
                hosted_by_this_app: false,
                can_maintain: entry
                    .grants
                    .as_ref()
                    .is_some_and(|s| s.grants("environment:maintain")),
                can_manage_providers: entry
                    .grants
                    .as_ref()
                    .is_some_and(|s| s.grants("providers:manage")),
                updating: is_updating(&owner.saved_id),
            }
        })
        .collect();
    collect(&machines)
}

impl UpdateControl {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let dialog = cx.new(|cx| dialog::UpdateDialog::new(state.clone(), cx));
        cx.set_global(Host {
            control: cx.weak_entity(),
            dialog: dialog.downgrade(),
        });
        let subscriptions = vec![cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                this.reconcile_handed_off(cx);
                this.sync(cx);
            }
        })];
        let mut this = Self {
            state,
            dialog,
            plan: Plan::default(),
            pill: None,
            dismissed: HashSet::new(),
            visible_after: None,
            connection: None,
            success_timer: None,
            run: None,
            stages: Vec::new(),
            captures: Vec::new(),
            capsule_open: false,
            capsule_timer: None,
            handoff_timer: None,
            active_runs: 0,
            run_updated_servers: Vec::new(),
            run_updated_providers: 0,
            run_provider_failures: Vec::new(),
            resumed_handoff: None,
            #[cfg(test)]
            last_saved_result: None,
            fixture_now_ms: None,
            fixture: false,
            _subscriptions: subscriptions,
            _grants: None,
        };
        if let Some(saved) = t3_client::store::load_ui_state(SAVED_RESULT_STORE_KEY) {
            this.restore_saved_result(saved, now_ms(), cx);
        }
        this.sync(cx);
        this
    }

    pub(super) fn is_updating(&self, environment_id: &str) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.is_updating_environment(environment_id))
    }

    fn current_now_ms(&self) -> i64 {
        self.fixture_now_ms.unwrap_or_else(now_ms)
    }

    fn show_result_toast(&self, result: &UpdateEverythingResult, cx: &mut Context<Self>) {
        let Some(view) = updates::update_everything_toast_view(result) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            let mut toast = Toast::new(
                if view.is_error {
                    ToastKind::Error
                } else {
                    ToastKind::Success
                },
                view.title,
            );
            if let Some(description) = view.description {
                toast = toast.description(description);
            }
            state.show_toast(toast, cx);
        });
    }

    fn persist_saved_result(&mut self, result: &UpdateEverythingResult) {
        let has_handed_off = self.run.as_ref().is_some_and(|run| {
            run.machines
                .iter()
                .any(|m| m.result.is_none() && m.handed_off_version.is_some())
        });
        if has_handed_off {
            let saved = SavedUpdateEverythingResult::from_run(
                self.run.as_ref(),
                &self.stages,
                result,
                now_ms(),
            );
            #[cfg(test)]
            {
                self.last_saved_result = Some(saved.clone());
            }
            t3_client::store::save_ui_state(SAVED_RESULT_STORE_KEY, &saved);
        } else {
            #[cfg(test)]
            {
                self.last_saved_result = None;
            }
            t3_client::store::save_ui_state(SAVED_RESULT_STORE_KEY, &serde_json::Value::Null);
        }
    }

    fn clear_run_outcomes(&mut self) {
        self.run_updated_servers.clear();
        self.run_updated_providers = 0;
        self.run_provider_failures.clear();
        self.resumed_handoff = None;
    }

    fn in_flight_result(&self) -> UpdateEverythingResult {
        let mut failures = self.run_provider_failures.clone();
        if let Some(run) = &self.run {
            for machine in &run.machines {
                if machine.result == Some(MachineResult::Failed)
                    && let Some(failure) = &machine.failure
                    && machine.provider_failure.as_ref() != Some(failure)
                {
                    failures.push(format!("{}: {failure}", machine.label));
                }
            }
        }
        UpdateEverythingResult {
            updated_servers: self.run_updated_servers.len(),
            updated_providers: self.run_updated_providers,
            failures,
            restarting: Vec::new(),
        }
    }

    pub(super) fn restore_saved_result(
        &mut self,
        value: serde_json::Value,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        t3_client::store::save_ui_state(SAVED_RESULT_STORE_KEY, &serde_json::Value::Null);
        let Some(saved) = updates::decode_fresh_saved_result(value, now_ms) else {
            return;
        };
        let toast_result = saved.into_toast_result();
        self.show_result_toast(&toast_result, cx);
        if let Some(run) = saved.into_resuming_run() {
            self.resumed_handoff = Some(ResumedHandoff {
                remaining: run.machines.len(),
                updated_servers: 0,
                failures: Vec::new(),
            });
            self.run = Some(run);
            self.reconcile_handed_off(cx);
            self.refresh_timers(cx);
        }
    }

    fn reconcile_handed_off(&mut self, cx: &mut Context<Self>) {
        self.reconcile_handed_off_at(self.current_now_ms(), cx);
    }

    fn reconcile_handed_off_at(&mut self, now: i64, cx: &mut Context<Self>) {
        if self.fixture {
            return;
        }
        let Some(run) = &self.run else {
            return;
        };
        let state = self.state.read(cx);
        let mut settlements = Vec::new();
        for machine in &run.machines {
            if machine.result.is_some() {
                continue;
            }
            let Some(target_version) = &machine.handed_off_version else {
                continue;
            };
            let resumed = state
                .fleet
                .environment(&machine.environment_id)
                .filter(|entry| {
                    entry.status == t3_client::supervisor::ConnectionStatus::Connected
                        && entry.summary.synchronized
                        && entry.summary.config_loaded
                })
                .and_then(|entry| entry.config.config.as_ref())
                .is_some_and(|config| {
                    !config.environment.server_version.trim().is_empty()
                        && !updates::behind(&config.environment.server_version, target_version)
                });
            if resumed {
                settlements.push((machine.environment_id.clone(), machine.label.clone(), None, true));
            } else if self.active_runs == 0 && now - machine.started_at_ms >= updates::HANDOFF_TIMEOUT_MS
            {
                settlements.push((
                    machine.environment_id.clone(),
                    machine.label.clone(),
                    Some("Timed out waiting for the server to resume after updating.".to_owned()),
                    false,
                ));
            }
        }
        if settlements.is_empty() {
            return;
        }
        for (id, label, failure, server_updated) in settlements {
            set_stage(&mut self.stages, &id, None);
            if let Some(tracker) = &mut self.resumed_handoff {
                if server_updated {
                    tracker.updated_servers += 1;
                } else if let Some(msg) = &failure {
                    tracker.failures.push(format!("{label}: {msg}"));
                }
                tracker.remaining = tracker.remaining.saturating_sub(1);
            }
            updates::settle_machine(&mut self.run, &id, failure, server_updated, None);
        }
        if self.resumed_handoff.as_ref().is_some_and(|t| t.remaining == 0)
            && let Some(tracker) = self.resumed_handoff.take()
        {
            self.show_result_toast(
                &UpdateEverythingResult {
                    updated_servers: tracker.updated_servers,
                    updated_providers: 0,
                    failures: tracker.failures,
                    restarting: Vec::new(),
                },
                cx,
            );
        }
        if self.run.is_none() {
            self.capsule_open = false;
            self.clear_run_outcomes();
            t3_client::store::save_ui_state(SAVED_RESULT_STORE_KEY, &serde_json::Value::Null);
        }
        self.refresh_timers(cx);
        cx.emit(());
        cx.notify();
    }

    fn refresh_timers(&mut self, cx: &mut Context<Self>) {
        if self.fixture {
            self.capsule_timer = None;
            self.handoff_timer = None;
            return;
        }
        let active = self
            .run
            .as_ref()
            .is_some_and(|run| run.has_active_machine(&self.stages));
        if !active {
            self.capsule_timer = None;
        } else if self.capsule_timer.is_none() {
            self.capsule_timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_secs(30))
                        .await;
                    let keep_going = this
                        .update(cx, |this, cx| {
                            let still_active = this
                                .run
                                .as_ref()
                                .is_some_and(|r| r.has_active_machine(&this.stages));
                            if !still_active {
                                this.capsule_timer = None;
                            }
                            cx.notify();
                            still_active
                        })
                        .unwrap_or(false);
                    if !keep_going {
                        break;
                    }
                }
            }));
        }

        let next_handoff_deadline = if self.active_runs == 0 {
            self.run.as_ref().and_then(|run| {
                run.machines
                    .iter()
                    .filter(|m| m.result.is_none() && m.handed_off_version.is_some())
                    .map(|m| m.started_at_ms + updates::HANDOFF_TIMEOUT_MS)
                    .min()
            })
        } else {
            None
        };
        if let Some(deadline) = next_handoff_deadline {
            let wait_ms = (deadline - self.current_now_ms()).max(0) as u64;
            self.handoff_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(wait_ms))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.handoff_timer = None;
                    this.reconcile_handed_off_at(deadline, cx);
                    this.sync(cx);
                });
            }));
        } else {
            self.handoff_timer = None;
        }
    }

    fn apply_event(&mut self, event: UpdateEvent, cx: &mut Context<Self>) {
        match event {
            UpdateEvent::ProvidersFinished {
                updated_providers,
                failures,
            } => {
                self.run_updated_providers += updated_providers;
                self.run_provider_failures.extend(failures);
                updates::begin_servers_phase(&mut self.run, now_ms());
                if self.run.is_none() {
                    self.capsule_open = false;
                    self.clear_run_outcomes();
                } else {
                    self.persist_saved_result(&self.in_flight_result());
                }
            }
            UpdateEvent::ServerStage {
                environment_id,
                stage,
            } => {
                set_stage(&mut self.stages, &environment_id, Some(stage));
            }
            UpdateEvent::ServerHandedOff {
                environment_id,
                target_version,
            } => {
                if let Some(run) = &mut self.run {
                    for machine in &mut run.machines {
                        if machine.environment_id == environment_id {
                            machine.handed_off_version = Some(target_version.clone());
                        }
                    }
                }
                self.persist_saved_result(&self.in_flight_result());
            }
            UpdateEvent::MachineSettled {
                environment_id,
                failure,
                server_updated,
                provider_failure,
            } => {
                set_stage(&mut self.stages, &environment_id, None);
                if server_updated && !self.run_updated_servers.contains(&environment_id) {
                    self.run_updated_servers.push(environment_id.clone());
                }
                updates::settle_machine(
                    &mut self.run,
                    &environment_id,
                    failure,
                    server_updated,
                    Some(provider_failure),
                );
                if self.run.is_none() {
                    self.capsule_open = false;
                    self.clear_run_outcomes();
                } else {
                    self.persist_saved_result(&self.in_flight_result());
                }
            }
        }
        self.refresh_timers(cx);
        self.sync(cx);
        cx.emit(());
        cx.notify();
    }

    fn merge_run(&mut self, incoming: UpdateEverythingRun) {
        let Some(existing) = &mut self.run else {
            self.run = Some(incoming);
            return;
        };
        for machine in incoming.machines {
            set_stage(&mut self.stages, &machine.environment_id, None);
            if let Some(slot) = existing
                .machines
                .iter_mut()
                .find(|m| m.environment_id == machine.environment_id)
            {
                *slot = machine;
            } else {
                existing.machines.push(machine);
            }
        }
        existing.track_switch = existing.track_switch || incoming.track_switch;
        if incoming.phase == UpdateRunPhase::Providers
            || existing
                .machines
                .iter()
                .any(|m| m.updates_providers && m.result.is_none())
        {
            existing.phase = UpdateRunPhase::Providers;
        }
    }

    pub(super) fn start_run(
        &mut self,
        plan: Plan,
        captures: Vec<CapturedUpdateOwner>,
        cx: &mut Context<Self>,
    ) {
        let incoming_plan = plan.clone();
        self.start_run_inner(
            &incoming_plan,
            false,
            captures,
            move |destinations, view, tx| {
                Box::pin(t3_client::updates::run_with_progress(
                    plan,
                    destinations,
                    view,
                    Some(tx),
                ))
            },
            cx,
        );
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn start_channel_switch(
        &mut self,
        switch: updates::ChannelSwitchPlan,
        release_channel: updates::UpdateTrack,
        captures: Vec<CapturedUpdateOwner>,
        cx: &mut Context<Self>,
    ) {
        let plan = switch.plan.clone();
        self.start_run_inner(
            &plan,
            true,
            captures,
            move |destinations, view, tx| {
                Box::pin(t3_client::updates::run_channel_switch_with_progress(
                    switch,
                    release_channel,
                    destinations,
                    view,
                    Some(tx),
                ))
            },
            cx,
        );
    }

    fn start_run_inner<F>(
        &mut self,
        plan: &Plan,
        track_switch: bool,
        captures: Vec<CapturedUpdateOwner>,
        runner: impl FnOnce(
                Vec<UpdateDestination>,
                tokio::sync::watch::Receiver<std::sync::Arc<t3_client::environments::FleetView>>,
                tokio::sync::mpsc::UnboundedSender<UpdateEvent>,
            ) -> F
            + Send
            + 'static,
        cx: &mut Context<Self>,
    ) where
        F: std::future::Future<Output = UpdateEverythingResult> + Send + 'static,
    {
        if self.run.is_none() {
            self.clear_run_outcomes();
        }
        let mut lookups = Vec::new();
        let (view, runtime) = {
            let state = self.state.read(cx);
            for capture in &captures {
                for (scope, needed) in [
                    (
                        "providers:manage",
                        plan.providers
                            .iter()
                            .any(|p| p.environment_id == capture.owner.saved_id),
                    ),
                    (
                        "environment:maintain",
                        track_switch
                            || plan
                                .servers
                                .iter()
                                .any(|s| s.environment_id == capture.owner.saved_id),
                    ),
                ] {
                    if needed {
                        let receipt = state.connections.destination(&capture.owner, scope);
                        lookups.push((capture.clone(), scope, receipt));
                    }
                }
            }
            (state.connections.view(), state.runtime().clone())
        };
        for capture in &captures {
            self.captures
                .retain(|c| c.owner.saved_id != capture.owner.saved_id);
            self.captures.push(capture.clone());
        }
        self.capsule_open = false;
        let incoming = UpdateEverythingRun::from_plan(plan, track_switch, now_ms());
        let env_ids: Vec<String> = incoming
            .machines
            .iter()
            .map(|m| m.environment_id.clone())
            .collect();
        self.merge_run(incoming);
        self.active_runs += 1;
        self.refresh_timers(cx);
        self.sync(cx);
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UpdateEvent>();
        let task = runtime.spawn(async move {
            let mut destinations = Vec::new();
            for (capture, scope, receipt) in lookups {
                destinations.push(UpdateDestination {
                    capture,
                    scope,
                    receipt: receipt.await,
                });
            }
            runner(destinations, view, tx).await
        });
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let _ = this.update(cx, |this, cx| this.apply_event(event, cx));
            }
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.active_runs = this.active_runs.saturating_sub(1);
                match result {
                    Ok(result) => {
                        this.persist_saved_result(&result);
                        this.show_result_toast(&result, cx);
                    }
                    Err(error) => {
                        let msg = error.to_string();
                        for id in &env_ids {
                            if this
                                .run
                                .as_ref()
                                .is_some_and(|r| r.is_updating_environment(id))
                            {
                                set_stage(&mut this.stages, id, None);
                                updates::settle_machine(
                                    &mut this.run,
                                    id,
                                    Some(msg.clone()),
                                    false,
                                    None,
                                );
                            }
                        }
                        if this.run.is_none() {
                            this.capsule_open = false;
                            this.clear_run_outcomes();
                        }
                        let failed_result = UpdateEverythingResult {
                            failures: vec![msg],
                            ..Default::default()
                        };
                        this.persist_saved_result(&failed_result);
                        this.show_result_toast(&failed_result, cx);
                    }
                }
                if this.run.is_none() {
                    this.clear_run_outcomes();
                }
                this.refresh_timers(cx);
                this.sync(cx);
                cx.emit(());
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn retry_failed(&mut self, cx: &mut Context<Self>) {
        if self.fixture {
            return;
        }
        let targets: Vec<(ServerTarget, Option<String>)> =
            updates::retry_failed_servers(&mut self.run, now_ms());
        if targets.is_empty() {
            return;
        }
        for (target, _) in &targets {
            set_stage(&mut self.stages, &target.environment_id, None);
        }
        let retry_plan = Plan {
            servers: targets.iter().map(|(s, _)| s.clone()).collect(),
            ..Default::default()
        };
        let capture_result = {
            let state = self.state.read(cx);
            t3_client::updates::capture(&retry_plan, &state.fleet).and_then(|captures| {
                for capture in &captures {
                    if let Some(prev) = self
                        .captures
                        .iter()
                        .find(|c| c.owner.saved_id == capture.owner.saved_id)
                        && (prev.owner.canonical_id != capture.owner.canonical_id
                            || prev.owner.generation != capture.owner.generation)
                    {
                        return Err(format!("{}: server owner changed", capture.owner.saved_id));
                    }
                }
                Ok(captures)
            })
        };
        let captures = match capture_result {
            Ok(captures) => captures,
            Err(error) => {
                let mut failures = Vec::new();
                for (target, _) in &targets {
                    updates::settle_machine(
                        &mut self.run,
                        &target.environment_id,
                        Some(error.clone()),
                        false,
                        None,
                    );
                    failures.push(format!("{}: {error}", target.label));
                }
                self.show_result_toast(
                    &UpdateEverythingResult {
                        failures,
                        ..Default::default()
                    },
                    cx,
                );
                self.refresh_timers(cx);
                self.sync(cx);
                cx.notify();
                return;
            }
        };
        let mut lookups = Vec::new();
        let (view, runtime) = {
            let state = self.state.read(cx);
            for capture in &captures {
                let receipt = state
                    .connections
                    .destination(&capture.owner, "environment:maintain");
                lookups.push((capture.clone(), "environment:maintain", receipt));
            }
            (state.connections.view(), state.runtime().clone())
        };
        for capture in &captures {
            self.captures
                .retain(|c| c.owner.saved_id != capture.owner.saved_id);
            self.captures.push(capture.clone());
        }
        self.active_runs += 1;
        self.refresh_timers(cx);
        self.sync(cx);
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UpdateEvent>();
        let retry_count = targets.len();
        let target_ids: Vec<String> = targets.iter().map(|(s, _)| s.environment_id.clone()).collect();
        let task = runtime.spawn(async move {
            let mut destinations = Vec::new();
            for (capture, scope, receipt) in lookups {
                destinations.push(UpdateDestination {
                    capture,
                    scope,
                    receipt: receipt.await,
                });
            }
            t3_client::updates::retry_servers_with_progress(targets, destinations, view, Some(tx))
                .await
        });
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let _ = this.update(cx, |this, cx| this.apply_event(event, cx));
            }
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.active_runs = this.active_runs.saturating_sub(1);
                let failures = match result {
                    Ok(failures) => failures,
                    Err(error) => {
                        let msg = error.to_string();
                        for id in &target_ids {
                            if this
                                .run
                                .as_ref()
                                .is_some_and(|r| r.is_updating_environment(id))
                            {
                                set_stage(&mut this.stages, id, None);
                                updates::settle_machine(
                                    &mut this.run,
                                    id,
                                    Some(msg.clone()),
                                    false,
                                    None,
                                );
                            }
                        }
                        vec![msg]
                    }
                };
                if this.run.is_none() {
                    this.capsule_open = false;
                }
                let updated_servers = retry_count.saturating_sub(failures.len());
                let summary = UpdateEverythingResult {
                    updated_servers,
                    updated_providers: 0,
                    failures,
                    restarting: Vec::new(),
                };
                if this.run.is_some() {
                    this.persist_saved_result(&this.in_flight_result());
                } else {
                    this.clear_run_outcomes();
                    this.persist_saved_result(&summary);
                }
                this.show_result_toast(&summary, cx);
                this.refresh_timers(cx);
                this.sync(cx);
                cx.emit(());
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn dismiss_settled(&mut self, cx: &mut Context<Self>) {
        updates::dismiss_settled_machines(&mut self.run);
        if let Some(run) = &self.run {
            self.stages
                .retain(|(id, _)| run.machines.iter().any(|m| &m.environment_id == id));
        } else {
            self.stages.clear();
            self.capsule_open = false;
            self.clear_run_outcomes();
        }
        self.refresh_timers(cx);
        self.sync(cx);
        cx.emit(());
        cx.notify();
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.fixture {
            return;
        }
        let state = self.state.read(cx);
        if self.connection != state.connection_id() {
            self.connection = state.connection_id();
            self.visible_after = None;
            self.dismissed.clear();
            self._grants = state.client_handle().map(|client| {
                let mut grants = client.session_state();
                cx.spawn(async move |this, cx| {
                    while grants.changed().await.is_ok() {
                        let _ = this.update(cx, |this, cx| this.sync(cx));
                    }
                })
            });
        }
        let state = self.state.read(cx);
        let providers = state
            .server_config
            .config
            .as_ref()
            .map(|c| c.providers.as_slice())
            .unwrap_or_default();
        if self.visible_after.is_none() {
            self.visible_after = providers
                .iter()
                .filter_map(|p| p.checked_at.0.as_ref())
                .max()
                .cloned();
        }
        let next_pill = if connected(state) {
            updates::provider_pill(providers, self.visible_after.as_deref(), &self.dismissed)
        } else {
            None
        };
        let next_plan = plan_with(state, |id| self.is_updating(id));
        let changed = self.plan != next_plan || self.pill != next_pill;
        self.plan = next_plan;
        if self.pill != next_pill {
            self.pill = next_pill;
            self.success_timer = None;
            if let Some(pill) = &self.pill
                && let Some(ms) = pill.dismiss_after_ms
            {
                let key = pill.key.clone();
                self.success_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.dismissed.insert(key);
                        this.sync(cx);
                    });
                }));
            }
        }
        if changed {
            cx.notify();
        }
    }
}

fn machine_status_icon(status: UpdateRunMachineStatus, slow: bool, p: &crate::theme::Palette) -> gpui::Svg {
    match status {
        UpdateRunMachineStatus::Done => Icon::CircleCheck.sized(px(14.)).text_color(hsla(p.success)),
        UpdateRunMachineStatus::Failed => Icon::TriangleAlert.sized(px(14.)).text_color(hsla(p.destructive)),
        UpdateRunMachineStatus::Waiting => Icon::Clock.sized(px(14.)).text_color(hsla(p.muted_foreground)),
        UpdateRunMachineStatus::Providers
        | UpdateRunMachineStatus::Downloading
        | UpdateRunMachineStatus::Restarting => Icon::LoaderCircle.sized(px(14.)).text_color(hsla(if slow {
            p.warning
        } else {
            p.update_foreground
        })),
    }
}

pub(super) fn capsule_popover_actions(
    run: &UpdateEverythingRun,
    stages: &[(String, Option<ServerUpdateStage>)],
) -> (bool, bool) {
    if run.has_active_machine(stages) {
        return (false, false);
    }
    let mut has_failed = false;
    let mut has_failed_server = false;
    for machine in &run.machines {
        let status = updates::resolve_machine_status(
            machine,
            run.phase,
            stage_for(stages, &machine.environment_id),
        );
        if status == UpdateRunMachineStatus::Failed {
            has_failed = true;
            if machine.server.is_some() {
                has_failed_server = true;
            }
        }
    }
    if !has_failed {
        return (false, false);
    }
    (!run.track_switch && has_failed_server, true)
}

fn render_capsule_popover(
    run: &UpdateEverythingRun,
    stages: &[(String, Option<ServerUpdateStage>)],
    now: i64,
    this: WeakEntity<UpdateControl>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let p = palette(cx);
    let mut rows = div().flex().flex_col().gap(rem(6.));
    for machine in &run.machines {
        let status = updates::resolve_machine_status(
            machine,
            run.phase,
            stage_for(stages, &machine.environment_id),
        );
        let elapsed_ms = now - machine.started_at_ms;
        let slow = status.is_working() && elapsed_ms >= updates::UPDATE_SLOW_AFTER_MS;
        let status_text = if status.is_working() {
            format!("{} · {}", status.label(), updates::format_update_elapsed(elapsed_ms))
        } else {
            status.label().to_owned()
        };
        let status_color = match status {
            UpdateRunMachineStatus::Failed => p.destructive,
            UpdateRunMachineStatus::Done => p.success,
            _ if slow => p.warning,
            _ => p.muted_foreground,
        };
        rows = rows.child(
            div()
                .flex()
                .flex_col()
                .gap(rem(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rem(6.))
                        .text_xs()
                        .child(machine_status_icon(status, slow, &p))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(hsla(p.popover_foreground))
                                .child(machine.label.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(hsla(status_color))
                                .child(status_text),
                        ),
                )
                .children(machine.failure.as_ref().map(|failure| {
                    div()
                        .pl(rem(20.))
                        .text_xs()
                        .text_color(hsla(p.destructive))
                        .child(failure.clone())
                })),
        );
    }

    let (show_retry, show_dismiss) = capsule_popover_actions(run, stages);
    let footer = if show_retry || show_dismiss {
        let mut bar = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(rem(6.))
            .pt(rem(6.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6));
        if show_retry {
            let retry_this = this.clone();
            bar = bar.child(keyboard_button(
                div()
                    .id("update-capsule-popover-retry")
                    .px(rem(8.))
                    .py(rem(4.))
                    .rounded(rem(6.))
                    .bg(hsla(p.destructive).opacity(0.14))
                    .text_color(hsla(p.destructive))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .child("Retry"),
                "update-capsule-popover-retry",
                true,
                move |_, cx| {
                    let _ = retry_this.update(cx, |this, cx| this.retry_failed(cx));
                },
                window,
                cx,
            ));
        }
        let dismiss_this = this.clone();
        bar = bar.child(keyboard_button(
            div()
                .id("update-capsule-popover-dismiss")
                .px(rem(8.))
                .py(rem(4.))
                .rounded(rem(6.))
                .bg(hsla(p.muted))
                .text_color(hsla(p.popover_foreground))
                .text_xs()
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .child("Dismiss"),
            "update-capsule-popover-dismiss",
            true,
            move |_, cx| {
                let _ = dismiss_this.update(cx, |this, cx| this.dismiss_settled(cx));
            },
            window,
            cx,
        ));
        Some(bar)
    } else {
        None
    };

    let close_this = this.clone();
    let panel = div()
        .id("update-capsule-popover")
        .debug_selector(|| "update-capsule-popover".to_owned())
        .relative()
        .occlude()
        .w(rem(272.))
        .rounded(rem(8.))
        .overflow_hidden()
        .text_color(hsla(p.popover_foreground))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::black().opacity(0.55),
            offset: gpui::point(px(0.), px(16.)),
            blur_radius: px(40.),
            spread_radius: px(-18.),
            inset: false,
        }])
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(crate::glass::backdrop(
            8.,
            hsla(p.dropdown_surface),
            hsla(p.dropdown_border),
            cx,
        ))
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .gap(rem(8.))
                .p(rem(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.muted_foreground))
                                .child("Updates"),
                        )
                        .child(keyboard_button(
                            div()
                                .id("update-capsule-close")
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(rem(4.))
                                .cursor_pointer()
                                .opacity(0.7)
                                .hover(|s| s.opacity(1.))
                                .child(Icon::X.sized(px(12.)).text_color(hsla(p.muted_foreground))),
                            "update-capsule-close",
                            true,
                            move |_, cx| {
                                let _ = close_this.update(cx, |this, cx| {
                                    this.capsule_open = false;
                                    cx.notify();
                                });
                            },
                            window,
                            cx,
                        )),
                )
                .child(rows)
                .children(footer),
        );

    div()
        .absolute()
        .top_0()
        .right_0()
        .child(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomRight)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().mb(rem(6.)).child(panel)),
            )
            .with_priority(3),
        )
        .into_any_element()
}

impl UpdateControl {
    fn key_boundary(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .key_context(CONTROL_CONTEXT)
            .on_action(|_: &FocusNextUpdate, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPreviousUpdate, window, cx| window.focus_prev(cx))
            .on_action(cx.listener(|this, _: &CloseCapsulePopover, _, cx| {
                if this.capsule_open {
                    this.capsule_open = false;
                    cx.notify();
                }
            }))
    }

    pub(crate) fn render_notices(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let denial = self
            .state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .and_then(|c| updates::managed_denial(c.managed_policy.as_ref(), Some(target_version())));
        if denial.is_none() && self.pill.is_none() {
            return None;
        }
        let p = palette(cx);
        let mut body = self
            .key_boundary(cx)
            .flex_none()
            .flex()
            .flex_col()
            .gap(rem(6.))
            .px(rem(8.));
        if let Some(reason) = denial {
            use crate::settings::controls::{Button, ButtonSize, ButtonVariant};
            body = body
                .child(crate::components::managed::ManagedControl::new(
                    "managed-server-update",
                    Some(reason.clone()),
                    Button::new(
                        "managed-server-update-button",
                        ButtonVariant::Outline,
                        ButtonSize::Compact,
                    )
                    .label("Update T3 Code"),
                ))
                .child(crate::components::managed::notice(reason, cx));
        }
        if let Some(view) = &self.pill {
            let (fill, color, icon) = match view.tone {
                Tone::Loading => (
                    hsla(p.sidebar_control_surface),
                    p.sidebar_foreground,
                    Icon::LoaderCircle,
                ),
                Tone::Success => (
                    hsla(p.sidebar_control_surface),
                    p.sidebar_foreground,
                    Icon::CircleCheck,
                ),
                Tone::Warning => (hsla(p.warning).opacity(0.12), p.warning, Icon::Download),
                Tone::Error => (
                    hsla(p.destructive).opacity(0.12),
                    p.destructive,
                    Icon::TriangleAlert,
                ),
            };
            let description = view.description.clone();
            let mut row = div()
                .w_full()
                .flex()
                .items_center()
                .rounded(rem(8.))
                .bg(fill)
                .text_color(hsla(color))
                .text_size(px(11.))
                .line_height(rem(16.));
            row = row.child(keyboard_button(
                div()
                    .id("provider-update-main")
                    .debug_selector(|| "provider-update-main".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(rem(8.))
                    .px(rem(8.))
                    .py(rem(6.))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.8))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(description.clone()).build(window, cx)
                    })
                    .child(icon.sized(px(14.)).text_color(hsla(color)))
                    .child(view.title.clone()),
                "provider-update-main",
                true,
                |_, cx| crate::settings::open(Section::Providers, cx),
                window,
                cx,
            ));
            if view.dismissible {
                let this = cx.weak_entity();
                row = row.child(keyboard_button(
                    div()
                        .id("dismiss-provider-update")
                        .size(px(20.))
                        .mr(rem(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(rem(6.))
                        .cursor_pointer()
                        .opacity(0.7)
                        .hover(|s| s.opacity(1.))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                "Dismiss until provider status changes",
                            )
                            .build(window, cx)
                        })
                        .child(Icon::X.sized(px(14.)).text_color(hsla(color))),
                    "dismiss-provider-update",
                    true,
                    move |window, cx| {
                        if window.last_input_was_keyboard() {
                            window.focus_next(cx);
                        }
                        let _ = this.update(cx, |this, cx| {
                            if let Some(pill) = &this.pill {
                                this.dismissed.insert(pill.key.clone());
                            }
                            if this.fixture {
                                this.pill = None;
                                cx.notify();
                            } else {
                                this.sync(cx);
                            }
                        });
                    },
                    window,
                    cx,
                ));
            }
            body = body.child(row);
        }
        Some(body.into_any_element())
    }

    pub(crate) fn render_footer_control(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let p = palette(cx);
        if let Some(run) = &self.run {
            let now = self.current_now_ms();
            let view = updates::describe_update_capsule(run, &self.stages, now);
            let (fill, border, text_color, icon) = match view.tone {
                CapsuleTone::Failed => (
                    hsla(p.destructive).opacity(0.12),
                    hsla(p.destructive).opacity(0.4),
                    p.destructive,
                    Icon::TriangleAlert,
                ),
                CapsuleTone::Slow => (
                    hsla(p.warning).opacity(0.12),
                    hsla(p.warning).opacity(0.4),
                    p.warning,
                    Icon::LoaderCircle,
                ),
                CapsuleTone::Default => (
                    hsla(p.sidebar_control_surface),
                    hsla(p.border).opacity(0.5),
                    p.sidebar_foreground,
                    Icon::LoaderCircle,
                ),
            };
            let trigger_label = if let Some((status, elapsed)) = &view.slow {
                format!("{} · {status} · {elapsed}", view.label)
            } else {
                view.label.clone()
            };
            let this = cx.weak_entity();
            let mut capsule_row = div()
                .id("update-capsule")
                .debug_selector(|| "update-capsule".to_owned())
                .w_full()
                .min_w_0()
                .h(px(32.))
                .overflow_hidden()
                .flex()
                .items_center()
                .rounded(rem(8.))
                .border_1()
                .border_color(border)
                .bg(fill)
                .text_color(hsla(text_color))
                .text_size(px(11.))
                .child(keyboard_button(
                    div()
                        .id("update-capsule-trigger")
                        .debug_selector(|| "update-capsule-trigger".to_owned())
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.85))
                        .child(icon.sized(px(14.)).text_color(hsla(text_color)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(trigger_label),
                        ),
                    "update-capsule-trigger",
                    true,
                    move |_, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.capsule_open = !this.capsule_open;
                            cx.notify();
                        });
                    },
                    window,
                    cx,
                ));
            if let Some(action) = view.action {
                let action_label = match action {
                    CapsuleAction::Retry => "Retry",
                    CapsuleAction::Dismiss => "Dismiss",
                };
                let action_this = cx.weak_entity();
                capsule_row = capsule_row.child(keyboard_button(
                    div()
                        .id("update-capsule-action")
                        .flex_none()
                        .mr(rem(4.))
                        .px(rem(6.))
                        .py(rem(3.))
                        .rounded(rem(6.))
                        .bg(hsla(p.destructive).opacity(0.18))
                        .text_color(hsla(p.destructive))
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.85))
                        .child(action_label),
                    "update-capsule-action",
                    true,
                    move |_, cx| {
                        let _ = action_this.update(cx, |this, cx| match action {
                            CapsuleAction::Retry => this.retry_failed(cx),
                            CapsuleAction::Dismiss => this.dismiss_settled(cx),
                        });
                    },
                    window,
                    cx,
                ));
            }
            let mut capsule_wrap = div().relative().w_full().min_w_0().child(capsule_row);
            if self.capsule_open {
                capsule_wrap = capsule_wrap.child(render_capsule_popover(
                    run,
                    &self.stages,
                    now,
                    cx.weak_entity(),
                    window,
                    cx,
                ));
            }
            return Some(
                self.key_boundary(cx)
                    .ml_auto()
                    .flex_1()
                    .min_w_0()
                    .child(capsule_wrap)
                    .into_any_element(),
            );
        }
        if self.plan.has_updates() {
            let label = self.plan.summary();
            let this = cx.weak_entity();
            let button = keyboard_button(
                div()
                    .id("update-everything")
                    .debug_selector(|| "update-everything".to_owned())
                    .size(px(32.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(hsla(p.sidebar_control_surface))
                    .text_color(hsla(p.sidebar_foreground))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(p.sidebar_row_hover)))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                    })
                    .child(
                        Icon::CircleArrowUp
                            .sized(px(16.))
                            .text_color(hsla(p.sidebar_foreground)),
                    ),
                "update-everything",
                true,
                move |window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        let plan = this.plan.clone();
                        this.dialog.update(cx, |dialog, cx| {
                            dialog.open(plan, this.fixture, window, cx)
                        });
                    });
                },
                window,
                cx,
            );
            return Some(
                self.key_boundary(cx)
                    .ml_auto()
                    .flex_none()
                    .child(button)
                    .into_any_element(),
            );
        }
        None
    }
}

impl Render for UpdateControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(rem(6.))
            .children(self.render_notices(window, cx))
            .children(
                self.render_footer_control(window, cx)
                    .map(|control| div().px(rem(8.)).flex().items_center().child(control)),
            )
    }
}
#[cfg(feature = "snapshot")]
pub fn show_fixture(value: serde_json::Value, window: &mut Window, cx: &mut App) {
    fixture::show(value, window, cx);
}
