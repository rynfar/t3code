use super::{
    Host, channel_switch_plan_with, dispatch_channel_switch, plan_with, raw_saved_update_track,
    set_saved_update_track, target_version,
};
use std::rc::Rc;
use t3_client::updates::CapturedUpdateOwner;
use crate::{
    app_state::{AppEvent, AppState},
    icons::Icon,
    theme::{hsla, palette, rem},
};
use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    Render, Styled, Subscription, Task, Window, deferred, div, prelude::*, px,
};
use t3_core::updates::{
    self, ChannelSwitchPlan, ChannelSwitchRelease, LocalUpdaterState, Plan, UpdateTrack,
    channel_switch_label, local_updater_stays_put, nothing_to_move, thread_warning,
};

pub struct UpdateDialog {
    state: Entity<AppState>,
    pub(super) plan: Option<Plan>,
    captures: Result<Vec<CapturedUpdateOwner>, String>,
    pub(super) fixture: bool,
    pub(super) checking: bool,
    pub(super) counts: Vec<(String, usize)>,
    expanded: bool,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    skipped_focus: FocusHandle,
    cancel_focus: FocusHandle,
    update_focus: FocusHandle,
    _subscription: Subscription,
}
impl UpdateDialog {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if this.plan.is_some()
                && matches!(
                    event,
                    AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase
                )
                && !this.fixture
            {
                if this.checking && matches!(event, AppEvent::ServerConfig | AppEvent::Phase) {
                    let control = cx.try_global::<Host>().and_then(|h| h.control.upgrade());
                    let state = this.state.read(cx);
                    let next_plan = plan_with(state, |id| {
                        control
                            .as_ref()
                            .is_some_and(|c| c.read(cx).is_updating(id))
                    });
                    if next_plan.has_updates() {
                        this.captures = t3_client::updates::capture(&next_plan, &state.fleet);
                        this.plan = Some(next_plan);
                    }
                }
                let counts = this.running_counts(cx);
                if counts != this.counts
                    || matches!(event, AppEvent::ServerConfig | AppEvent::Phase)
                {
                    this.counts = counts;
                    cx.notify();
                }
            }
        });
        Self {
            state,
            plan: None,
            captures: Ok(Vec::new()),
            fixture: false,
            checking: false,
            counts: Vec::new(),
            expanded: false,
            focus: cx.focus_handle(),
            previous_focus: None,
            skipped_focus: cx.focus_handle().tab_stop(true),
            cancel_focus: cx.focus_handle().tab_stop(true),
            update_focus: cx.focus_handle().tab_stop(true),
            _subscription: subscription,
        }
    }
    pub fn open(&mut self, plan: Plan, fixture: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = Some(plan);
        self.fixture = fixture;
        self.checking = false;
        self.expanded = false;
        self.captures = t3_client::updates::capture(self.plan.as_ref().unwrap(), &self.state.read(cx).fleet);
        self.counts = self.running_counts(cx);
        self.previous_focus = window.focused(cx);
        window.focus(&self.cancel_focus, cx);
        cx.notify();
    }
    pub(super) fn set_checking(&mut self, checking: bool, cx: &mut Context<Self>) {
        if self.checking != checking {
            self.checking = checking;
            cx.notify();
        }
    }
    fn running_counts(&self, cx: &App) -> Vec<(String, usize)> {
        let Some(plan) = &self.plan else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        plan.restarts.iter().map(|restart| {
            let count = self.captures.as_ref().ok().and_then(|captures| captures.iter().find(|c| c.owner.saved_id == restart.environment_id))
                .and_then(|capture| state.fleet.environment(&restart.environment_id).filter(|entry| entry.summary.owner == capture.owner))
                .map_or(0, |entry| entry.summary.running_thread_count);
            (restart.environment_id.clone(), count)
        }).collect()
    }
    pub(super) fn disabled_reason(&self, cx: &App) -> Option<&'static str> {
        if self.fixture {
            return None;
        }
        let state = self.state.read(cx);
        let Some(opening) = &self.plan else {
            return Some("No updates are selected.");
        };
        let control = cx.try_global::<Host>().and_then(|h| h.control.upgrade());
        let current_plan = plan_with(state, |id| {
            control
                .as_ref()
                .is_some_and(|c| c.read(cx).is_updating(id))
        });
        if opening != &current_plan
            || self.captures.as_ref().ok()
                != t3_client::updates::capture(opening, &state.fleet).as_ref().ok()
            || self.captures.is_err()
        {
            return Some("Updates changed. Close this dialog and review them again.");
        }
        None
    }
    fn cycle_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut stops = Vec::new();
        if self
            .plan
            .as_ref()
            .is_some_and(|p| p.rows.iter().any(|r| r.skipped.is_some()))
        {
            stops.push(&self.skipped_focus);
        }
        stops.push(&self.cancel_focus);
        if self.disabled_reason(cx).is_none() {
            stops.push(&self.update_focus);
        }
        let index = stops
            .iter()
            .position(|focus| focus.is_focused(window))
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % stops.len()
        } else {
            (index + stops.len() - 1) % stops.len()
        };
        window.focus(stops[next], cx);
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = None;
        self.checking = false;
        self.captures = Ok(Vec::new());
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        } else {
            window.blur();
        }
        cx.notify();
    }
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fixture {
            self.close(window, cx);
            return;
        }
        if self.disabled_reason(cx).is_some() {
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let Ok(captures) = self.captures.clone() else {
            return;
        };
        let control = cx.global::<Host>().control.clone();
        self.close(window, cx);
        if let Some(control) = control.upgrade() {
            control.update(cx, |control, cx| control.start_run(plan, captures, cx));
        }
    }
}
impl Render for UpdateDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = self.plan.as_ref() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let title = plan.title();
        let mut description = plan
            .version()
            .map(|v| format!("T3 Code {v}"))
            .unwrap_or_default();
        let n = plan.providers.len();
        if n > 0 {
            if !description.is_empty() {
                description.push_str(" · ");
            }
            description.push_str(&format!(
                "{n} provider {}",
                if n == 1 { "update" } else { "updates" }
            ));
        }
        if self.checking {
            if !description.is_empty() {
                description.push_str(" · ");
            }
            description.push_str(crate::i18n::t(cx, "Checking for more…"));
        }
        let mut rows = div()
            .id("update-machine-rows")
            .max_h(rem(360.))
            .overflow_y_scroll()
            .px(rem(24.))
            .pb(rem(16.))
            .flex()
            .flex_col()
            .gap(rem(6.));
        let cells = |label: String, app: String, providers: String, count: usize| {
            div()
                .flex()
                .gap(rem(16.))
                .py(rem(6.))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.5))
                .text_size(rem(14.))
                .child(
                    div()
                        .w(rem(128.))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .child(div().w(rem(84.)).child(app))
                .child(div().flex_1().min_w_0().child(providers))
                .child(
                    div()
                        .w(rem(70.))
                        .text_color(hsla(p.warning))
                        .child(if count > 0 {
                            format!("{count} running")
                        } else {
                            String::new()
                        }),
                )
        };
        rows = rows.child(
            div()
                .flex()
                .gap(rem(16.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .border_b_1()
                .border_color(hsla(p.border))
                .pb(rem(6.))
                .child(div().w(rem(128.)).child(crate::i18n::t(cx, "Machine")))
                .child(div().w(rem(84.)).child("T3 Code"))
                .child(div().flex_1().child(crate::i18n::t(cx, "Providers")))
                .child(div().w(rem(70.))),
        );
        for row in plan.updating_rows() {
            let count = self
                .counts
                .iter()
                .find(|(id, _)| id == &row.environment_id)
                .map_or(0, |(_, n)| *n);
            rows = rows.child(cells(
                row.label.clone(),
                row.app.clone().unwrap_or_else(|| {
                    if row.skipped.is_some() {
                        crate::i18n::t(cx, "Skipped").into()
                    } else {
                        String::new()
                    }
                }),
                row.providers.join(", "),
                count,
            ));
        }
        let skipped: Vec<_> = plan.rows.iter().filter(|r| r.skipped.is_some()).collect();
        if !skipped.is_empty() {
            rows = rows.child(
                div()
                    .id("update-skipped")
                    .track_focus(&self.skipped_focus)
                    .focus_visible(|s| s.text_color(hsla(p.foreground)))
                    .flex()
                    .items_center()
                    .gap(rem(4.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .cursor_pointer()
                    .child(
                        if self.expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        }
                        .sized(px(14.)),
                    )
                    .child(format!("{} skipped", skipped.len()))
                    .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                        if event.is_keyboard() {
                            return;
                        }
                        this.expanded = !this.expanded;
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if crate::settings::controls::activates(event) {
                            cx.stop_propagation();
                            this.expanded = !this.expanded;
                            cx.notify();
                        }
                    })),
            );
            if self.expanded {
                for row in skipped {
                    rows = rows.child(
                        div()
                            .pl(rem(18.))
                            .flex()
                            .gap(rem(12.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(div().w(rem(128.)).child(row.label.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .child(row.skipped.clone().unwrap_or_default()),
                            ),
                    );
                }
            }
        }
        let mut stopped = 0;
        let mut continuing = 0;
        for restart in &plan.restarts {
            let n = self
                .counts
                .iter()
                .find(|(id, _)| id == &restart.environment_id)
                .map_or(0, |(_, n)| *n);
            if restart.continues_threads {
                continuing += n;
            } else {
                stopped += n;
            }
        }
        let disabled = self.disabled_reason(cx);
        let warning = disabled
            .map(str::to_owned)
            .or_else(|| thread_warning(stopped, continuing));
        let button = |id, label, primary, focus: &FocusHandle| {
            div()
                .id(id)
                .track_focus(focus)
                .h(rem(32.))
                .px(rem(11.))
                .flex()
                .flex_none()
                .items_center()
                .rounded(rem(8.))
                .border_1()
                .border_color(hsla(if primary { p.primary } else { p.input }))
                .bg(hsla(if primary { p.primary } else { p.popover }))
                .text_color(hsla(if primary {
                    p.primary_foreground
                } else {
                    p.foreground
                }))
                .text_sm()
                .cursor_pointer()
                .focus_visible(|s| {
                    s.shadow(vec![gpui::BoxShadow {
                        color: hsla(p.ring),
                        offset: gpui::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    }])
                })
                .child(label)
        };
        deferred(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(rem(16.))
                .bg(hsla(p.dialog_backdrop))
                .child(
                    div()
                        .id("update-everything-dialog")
                        .key_context(super::DIALOG_CONTEXT)
                        .track_focus(&self.focus)
                        .on_action(cx.listener(|this, _: &super::FocusNextUpdate, window, cx| {
                            this.cycle_focus(true, window, cx)
                        }))
                        .on_action(cx.listener(
                            |this, _: &super::FocusPreviousUpdate, window, cx| {
                                this.cycle_focus(false, window, cx)
                            },
                        ))
                        .relative()
                        .w_full()
                        .max_w(rem(576.))
                        .rounded(rem(16.))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .child(
                            div()
                                .p(rem(24.))
                                .flex()
                                .flex_col()
                                .gap(rem(8.))
                                .child(
                                    div()
                                        .text_xl()
                                        .text_color(hsla(p.popover_foreground))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(description),
                                ),
                        )
                        .child(rows)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(rem(8.))
                                .px(rem(24.))
                                .py(rem(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted))
                                .rounded_b(rem(15.))
                                .children(warning.map(|s| {
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(s)
                                }))
                                .child(
                                    button("updates-cancel", crate::i18n::t(cx, "Cancel"), false, &self.cancel_focus)
                                        .on_click(cx.listener(
                                            |this, event: &gpui::ClickEvent, window, cx| {
                                                if !event.is_keyboard() {
                                                    this.close(window, cx);
                                                }
                                            },
                                        ))
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, window, cx| {
                                                if crate::settings::controls::activates(event) {
                                                    cx.stop_propagation();
                                                    this.close(window, cx);
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    button(
                                        "updates-confirm",
                                        crate::i18n::t(cx, "Update"),
                                        true,
                                        &self.update_focus.clone().tab_stop(disabled.is_none()),
                                    )
                                    .when(disabled.is_some(), |s| s.opacity(0.5))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                                    )
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if crate::settings::controls::activates(event) {
                                                cx.stop_propagation();
                                                this.confirm(window, cx);
                                            }
                                        },
                                    )),
                                ),
                        ),
                ),
        )
        .with_priority(10)
        .into_any_element()
    }
}

#[cfg(not(test))]
const NPM_REGISTRY_BASE: &str = "https://registry.npmjs.org";
#[cfg(not(test))]
const NPM_MANIFEST_MAX_BYTES: usize = 256 * 1024;
const SERVER_GET_NPM_RELEASE: &str = "server.getNpmRelease";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TrackSwitchPhase {
    Loading,
    NotFound,
    Error(String),
    Ready {
        release: ChannelSwitchRelease,
        switch: ChannelSwitchPlan,
        captures: Result<Vec<CapturedUpdateOwner>, String>,
    },
}

type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Ports `UpdateTrackSwitchDialog.tsx`: resolves the target dist-tag release (`latest` / `nightly`),
/// previews which connected machines will move or be skipped, persists the saved track in `ui_state`,
/// and dispatches `server.updateServer` for eligible connected machines.
pub(crate) struct UpdateTrackSwitchDialog {
    state: Entity<AppState>,
    channel: UpdateTrack,
    pub(crate) phase: TrackSwitchPhase,
    pub(crate) counts: Vec<(String, usize)>,
    expanded: bool,
    on_close: CloseHandler,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    skipped_focus: FocusHandle,
    cancel_focus: FocusHandle,
    confirm_focus: FocusHandle,
    _subscription: Subscription,
    _fetch: Option<Task<()>>,
}

impl UpdateTrackSwitchDialog {
    pub(crate) fn new(
        state: Entity<AppState>,
        channel: UpdateTrack,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase)
                && matches!(this.phase, TrackSwitchPhase::Ready { .. })
            {
                let counts = this.running_counts(cx);
                if counts != this.counts || matches!(event, AppEvent::ServerConfig | AppEvent::Phase)
                {
                    this.counts = counts;
                    cx.notify();
                }
            }
        });
        let previous_focus = window.focused(cx);
        let cancel_focus = cx.focus_handle().tab_stop(true);
        window.focus(&cancel_focus, cx);
        let mut this = Self {
            state,
            channel,
            phase: TrackSwitchPhase::Loading,
            counts: Vec::new(),
            expanded: false,
            on_close: Rc::new(on_close),
            focus: cx.focus_handle(),
            previous_focus,
            skipped_focus: cx.focus_handle().tab_stop(true),
            cancel_focus,
            confirm_focus: cx.focus_handle().tab_stop(true),
            _subscription: subscription,
            _fetch: None,
        };
        this.start_fetch(cx);
        this
    }

    fn start_fetch(&mut self, cx: &mut Context<Self>) {
        let channel = self.channel;
        let (lookup, runtime) = {
            let state = self.state.read(cx);
            let active_id = state.environments.active.as_deref();
            let candidate = active_id
                .and_then(|id| state.fleet.environment(id))
                .filter(|entry| {
                    entry.status == t3_client::supervisor::ConnectionStatus::Connected
                        && entry.summary.config_loaded
                })
                .or_else(|| {
                    state
                        .fleet
                        .environments
                        .iter()
                        .find(|entry| {
                            entry.status == t3_client::supervisor::ConnectionStatus::Connected
                                && entry.summary.config_loaded
                        })
                        .map(std::ops::Deref::deref)
                });
            let lookup = candidate.map(|entry| {
                (
                    entry.summary.owner.saved_id.clone(),
                    state.connections.client(&entry.summary.owner.saved_id),
                )
            });
            (lookup, state.runtime().clone())
        };
        let Some((_env_id, client_fut)) = lookup else {
            self.phase = TrackSwitchPhase::Error("Connect to a server to check releases.".into());
            cx.notify();
            return;
        };
        let task = runtime.spawn(async move {
            let Some(client) = client_fut.await else {
                return Err("Connect to a server to check releases.".to_owned());
            };
            match client
                .request(
                    SERVER_GET_NPM_RELEASE,
                    serde_json::json!({ "spec": channel.as_str() }),
                )
                .await
            {
                Ok(value) => updates::parse_npm_release_response(&value, channel),
                Err(error) => {
                    #[cfg(not(test))]
                    {
                        let fallback_eligible = matches!(
                            &error,
                            t3_client::session::RpcError::Failure(f)
                                if f.message.to_ascii_lowercase().contains("unknown")
                                    || f.message.to_ascii_lowercase().contains("not found")
                        );
                        if fallback_eligible {
                            let path = format!("/t3/{}", channel.as_str());
                            let bytes = t3_client::asset::fetch_asset(
                                NPM_REGISTRY_BASE,
                                &path,
                                NPM_MANIFEST_MAX_BYTES,
                            )
                            .await
                            .map_err(|_| {
                                "Could not check the npm registry. Check your connection and try again."
                                    .to_owned()
                            })?;
                            let json: serde_json::Value = serde_json::from_slice(&bytes)
                                .map_err(|_| "npm returned invalid release metadata.".to_owned())?;
                            return updates::parse_npm_release_response(&json, channel);
                        }
                    }
                    let msg = match error {
                        t3_client::session::RpcError::Failure(f) if !f.message.trim().is_empty() => {
                            f.message
                        }
                        _ => "Could not check this environment's npm registry. Check the connection and registry settings.".to_owned(),
                    };
                    Err(msg)
                }
            }
        });
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| {
                Err(
                    "Could not check this environment's npm registry. Check the connection and registry settings."
                        .to_owned(),
                )
            });
            let _ = this.update(cx, |this, cx| {
                this._fetch = None;
                this.apply_release_result(result, cx);
            });
        }));
    }

    pub(crate) fn apply_release_result(
        &mut self,
        result: Result<Option<ChannelSwitchRelease>, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(release)) => {
                let control = cx.try_global::<Host>().and_then(|h| h.control.upgrade());
                let state = self.state.read(cx);
                let switch = channel_switch_plan_with(state, &release, |id| {
                    control
                        .as_ref()
                        .is_some_and(|c| c.read(cx).is_updating(id))
                });
                let captures = t3_client::updates::capture_channel_switch(&switch, &state.fleet);
                self.phase = TrackSwitchPhase::Ready {
                    release,
                    switch,
                    captures,
                };
                self.counts = self.running_counts(cx);
            }
            Ok(None) => {
                self.phase = TrackSwitchPhase::NotFound;
                self.counts.clear();
            }
            Err(message) => {
                self.phase = TrackSwitchPhase::Error(message);
                self.counts.clear();
            }
        }
        cx.notify();
    }

    fn running_counts(&self, cx: &App) -> Vec<(String, usize)> {
        let TrackSwitchPhase::Ready {
            switch, captures, ..
        } = &self.phase
        else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        switch
            .plan
            .restarts
            .iter()
            .map(|restart| {
                let count = captures
                    .as_ref()
                    .ok()
                    .and_then(|caps| {
                        caps.iter()
                            .find(|c| c.owner.saved_id == restart.environment_id)
                    })
                    .and_then(|capture| {
                        state
                            .fleet
                            .environment(&restart.environment_id)
                            .filter(|entry| entry.summary.owner == capture.owner)
                    })
                    .map_or(0, |entry| entry.summary.running_thread_count);
                (restart.environment_id.clone(), count)
            })
            .collect()
    }

    fn idle_outcome(&self, cx: &App) -> Option<(updates::NothingToMove, bool, bool)> {
        let TrackSwitchPhase::Ready {
            release, switch, ..
        } = &self.phase
        else {
            return None;
        };
        let crosses_local = UpdateTrack::from_version(target_version()) != release.channel;
        let updates_off = local_updater_stays_put(Some(LocalUpdaterState::NATIVE), release.channel);
        let switches_local = crosses_local && !updates_off;
        let saved_channel = raw_saved_update_track(cx);
        let idle = nothing_to_move(
            release.channel,
            saved_channel,
            switches_local,
            crosses_local,
            &switch.plan.rows,
            !switch.targets.is_empty(),
        );
        Some((idle, crosses_local, updates_off))
    }

    pub(crate) fn disabled_reason(&self, cx: &App) -> Option<String> {
        let TrackSwitchPhase::Ready {
            release,
            switch,
            captures,
        } = &self.phase
        else {
            return Some("Checking releases…".into());
        };
        let (idle, _, _) = self.idle_outcome(cx)?;
        if let Some(reason) = idle.disabled_reason {
            return Some(reason);
        }
        let state = self.state.read(cx);
        let control = cx.try_global::<Host>().and_then(|h| h.control.upgrade());
        let current = channel_switch_plan_with(state, release, |id| {
            control
                .as_ref()
                .is_some_and(|c| c.read(cx).is_updating(id))
        });
        if switch != &current
            || captures.as_ref().ok()
                != t3_client::updates::capture_channel_switch(switch, &state.fleet)
                    .as_ref()
                    .ok()
            || captures.is_err()
        {
            return Some("Updates changed. Close this dialog and review them again.".into());
        }
        None
    }

    pub(crate) fn close_without_callback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._fetch = None;
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus, cx);
        } else {
            window.blur();
        }
    }

    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_without_callback(window, cx);
        let on_close = self.on_close.clone();
        on_close(window, cx);
    }

    pub(crate) fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled_reason(cx).is_some() {
            return;
        }
        let TrackSwitchPhase::Ready {
            release,
            switch,
            captures,
        } = &self.phase
        else {
            return;
        };
        let channel = release.channel;
        let switch = switch.clone();
        let captures = captures.clone().unwrap_or_default();
        set_saved_update_track(channel, cx);
        if !switch.targets.is_empty() {
            dispatch_channel_switch(&self.state, switch, channel, captures, cx);
        }
        self.close(window, cx);
    }

    fn cycle_focus(&self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut stops = Vec::new();
        if let TrackSwitchPhase::Ready { switch, .. } = &self.phase
            && switch.plan.rows.iter().any(|r| r.skipped.is_some())
        {
            stops.push(&self.skipped_focus);
        }
        stops.push(&self.cancel_focus);
        if matches!(self.phase, TrackSwitchPhase::Ready { .. }) && self.disabled_reason(cx).is_none()
        {
            stops.push(&self.confirm_focus);
        }
        let index = stops
            .iter()
            .position(|focus| focus.is_focused(window))
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % stops.len()
        } else {
            (index + stops.len() - 1) % stops.len()
        };
        window.focus(stops[next], cx);
    }
}

impl Render for UpdateTrackSwitchDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let label = crate::i18n::t(cx, channel_switch_label(self.channel));
        let switch_to_title = crate::i18n::t_fmt(cx, "Switch to {label}", &[("label", label.into())]);
        let switch_to_question = crate::i18n::t_fmt(cx, "Switch to {label}?", &[("label", label.into())]);
        let button = |id, text: String, primary: bool, focus: &FocusHandle| {
            div()
                .id(id)
                .track_focus(focus)
                .h(rem(32.))
                .px(rem(11.))
                .flex()
                .flex_none()
                .items_center()
                .rounded(rem(8.))
                .border_1()
                .border_color(hsla(if primary { p.primary } else { p.input }))
                .bg(hsla(if primary { p.primary } else { p.popover }))
                .text_color(hsla(if primary {
                    p.primary_foreground
                } else {
                    p.foreground
                }))
                .text_sm()
                .cursor_pointer()
                .focus_visible(|s| {
                    s.shadow(vec![gpui::BoxShadow {
                        color: hsla(p.ring),
                        offset: gpui::point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    }])
                })
                .child(text)
        };

        let (title, description, body_rows, warning, show_confirm, confirm_disabled) =
            match &self.phase {
                TrackSwitchPhase::Loading => (
                    switch_to_title.clone(),
                    crate::i18n::t(cx, "Checking releases…").to_owned(),
                    None,
                    None,
                    false,
                    true,
                ),
                TrackSwitchPhase::NotFound => (
                    switch_to_title.clone(),
                    crate::i18n::t_fmt(
                        cx,
                        "Could not find the latest {label} release in this environment's npm registry.",
                        &[("label", label.into())],
                    ),
                    None,
                    None,
                    false,
                    true,
                ),
                TrackSwitchPhase::Error(message) => (
                    switch_to_title.clone(),
                    message.clone(),
                    None,
                    None,
                    false,
                    true,
                ),
                TrackSwitchPhase::Ready {
                    release, switch, ..
                } => {
                    let plan = &switch.plan;
                    let (idle, crosses_local, updates_off) = self.idle_outcome(cx).unwrap();
                    let mut notes = Vec::new();
                    if let Some(note) = idle.note {
                        notes.push(note);
                    }
                    if crosses_local && updates_off {
                        notes.push(
                            "This app's updates are off here, so reinstall it yourself to switch."
                                .to_owned(),
                        );
                    }
                    let mut rows = div()
                        .id("track-switch-rows")
                        .max_h(rem(360.))
                        .overflow_y_scroll()
                        .px(rem(24.))
                        .pb(rem(16.))
                        .flex()
                        .flex_col()
                        .gap(rem(6.));
                    let updating: Vec<_> = plan.updating_rows().collect();
                    if !updating.is_empty() {
                        rows = rows.child(
                            div()
                                .flex()
                                .gap(rem(16.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .border_b_1()
                                .border_color(hsla(p.border))
                                .pb(rem(6.))
                                .child(div().w(rem(160.)).child(crate::i18n::t(cx, "Machine")))
                                .child(div().flex_1().child("T3 Code"))
                                .child(div().w(rem(70.))),
                        );
                        for row in updating {
                            let count = self
                                .counts
                                .iter()
                                .find(|(id, _)| id == &row.environment_id)
                                .map_or(0, |(_, n)| *n);
                            rows = rows.child(
                                div()
                                    .flex()
                                    .gap(rem(16.))
                                    .py(rem(6.))
                                    .border_b_1()
                                    .border_color(hsla(p.border).opacity(0.5))
                                    .text_size(rem(14.))
                                    .child(
                                        div()
                                            .w(rem(160.))
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(row.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(row.app.clone().unwrap_or_default()),
                                    )
                                    .child(
                                        div()
                                            .w(rem(70.))
                                            .text_color(hsla(p.warning))
                                            .child(if count > 0 {
                                                format!("{count} running")
                                            } else {
                                                String::new()
                                            }),
                                    ),
                            );
                        }
                    }
                    let skipped: Vec<_> =
                        plan.rows.iter().filter(|r| r.skipped.is_some()).collect();
                    if !skipped.is_empty() {
                        rows = rows.child(
                            div()
                                .id("track-switch-skipped")
                                .debug_selector(|| "track-switch-skipped".to_owned())
                                .track_focus(&self.skipped_focus)
                                .focus_visible(|s| s.text_color(hsla(p.foreground)))
                                .flex()
                                .items_center()
                                .gap(rem(4.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .cursor_pointer()
                                .child(
                                    if self.expanded {
                                        Icon::ChevronDown
                                    } else {
                                        Icon::ChevronRight
                                    }
                                    .sized(px(14.)),
                                )
                                .child(format!("{} skipped", skipped.len()))
                                .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                    if event.is_keyboard() {
                                        return;
                                    }
                                    this.expanded = !this.expanded;
                                    cx.notify();
                                }))
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if crate::settings::controls::activates(event) {
                                        cx.stop_propagation();
                                        this.expanded = !this.expanded;
                                        cx.notify();
                                    }
                                })),
                        );
                        if self.expanded {
                            for row in skipped {
                                rows = rows.child(
                                    div()
                                        .pl(rem(18.))
                                        .flex()
                                        .gap(rem(12.))
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(div().w(rem(140.)).child(row.label.clone()))
                                        .child(
                                            div()
                                                .flex_1()
                                                .child(row.skipped.clone().unwrap_or_default()),
                                        ),
                                );
                            }
                        }
                    }
                    for note in notes {
                        rows = rows.child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(note),
                        );
                    }
                    let mut stopped = 0;
                    let mut continuing = 0;
                    for restart in &plan.restarts {
                        let n = self
                            .counts
                            .iter()
                            .find(|(id, _)| id == &restart.environment_id)
                            .map_or(0, |(_, n)| *n);
                        if restart.continues_threads {
                            continuing += n;
                        } else {
                            stopped += n;
                        }
                    }
                    let disabled = self.disabled_reason(cx);
                    let is_disabled = disabled.is_some();
                    let warning = disabled.or_else(|| thread_warning(stopped, continuing));
                    (
                        switch_to_question,
                        format!("T3 Code {}", release.version),
                        Some(rows),
                        warning,
                        true,
                        is_disabled,
                    )
                }
            };

        deferred(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .p(rem(16.))
                .bg(hsla(p.dialog_backdrop))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close(window, cx)),
                )
                .child(
                    div()
                        .id("update-track-switch-dialog")
                        .debug_selector(|| "update-track-switch-dialog".to_owned())
                        .key_context(super::DIALOG_CONTEXT)
                        .track_focus(&self.focus)
                        .on_action(cx.listener(|this, _: &super::FocusNextUpdate, window, cx| {
                            this.cycle_focus(true, window, cx)
                        }))
                        .on_action(cx.listener(
                            |this, _: &super::FocusPreviousUpdate, window, cx| {
                                this.cycle_focus(false, window, cx)
                            },
                        ))
                        .relative()
                        .w_full()
                        .max_w(rem(576.))
                        .rounded(rem(16.))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(crate::glass::backdrop(
                            16.,
                            hsla(p.dialog_surface),
                            hsla(p.dialog_border),
                            cx,
                        ))
                        .child(
                            div()
                                .p(rem(24.))
                                .flex()
                                .flex_col()
                                .gap(rem(8.))
                                .child(
                                    div()
                                        .text_xl()
                                        .text_color(hsla(p.popover_foreground))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(description),
                                ),
                        )
                        .children(body_rows)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(rem(8.))
                                .px(rem(24.))
                                .py(rem(16.))
                                .border_t_1()
                                .border_color(hsla(p.border))
                                .bg(hsla(p.muted))
                                .rounded_b(rem(15.))
                                .children(warning.map(|s| {
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(s)
                                }))
                                .child(
                                    button(
                                        "track-switch-cancel",
                                        crate::i18n::t(cx, "Cancel").to_owned(),
                                        false,
                                        &self.cancel_focus,
                                    )
                                    .debug_selector(|| "track-switch-cancel".to_owned())
                                    .on_click(cx.listener(
                                        |this, event: &gpui::ClickEvent, window, cx| {
                                            if !event.is_keyboard() {
                                                this.close(window, cx);
                                            }
                                        },
                                    ))
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if crate::settings::controls::activates(event) {
                                                cx.stop_propagation();
                                                this.close(window, cx);
                                            }
                                        },
                                    )),
                                )
                                .when(show_confirm, |this| {
                                    this.child(
                                        button(
                                            "track-switch-confirm",
                                            switch_to_title,
                                            true,
                                            &self.confirm_focus.clone().tab_stop(!confirm_disabled),
                                        )
                                        .debug_selector(|| "track-switch-confirm".to_owned())
                                        .when(confirm_disabled, |s| s.opacity(0.5))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm(window, cx)
                                        }))
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, window, cx| {
                                                if crate::settings::controls::activates(event) {
                                                    cx.stop_propagation();
                                                    this.confirm(window, cx);
                                                }
                                            },
                                        )),
                                    )
                                }),
                        ),
                ),
        )
        .with_priority(10)
        .into_any_element()
    }
}
