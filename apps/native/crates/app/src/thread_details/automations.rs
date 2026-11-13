//! `ThreadAutomationsPanel` (`apps/web/src/components/chat/ThreadAutomationsPanel.tsx`):
//! scheduled tasks bound to the open thread, with CalendarClock + status dot, schedule line,
//! Manage/Edit jump to Settings -> Scheduled Tasks, Run now, and Enable/Pause switch.

use std::time::Duration;

use chrono::{Local, TimeZone};
use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::managed_policy::{Capability, capability_denial};
use t3_core::settings::scheduled_tasks::{RunStatus, ScheduledTask, decode_list, id_payload, method, set_enabled_payload};
use t3_core::thread_details::{automation_schedule_line, tasks_for_thread};

use super::controls::{hover_fill, section_with_action};
use super::view::DetailsView;
use crate::app_state::{AppState, Phase};
use crate::icons::Icon;
use crate::right_panel;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const AUTHORIZATION_ERROR: &str = "EnvironmentAuthorizationError";

struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn once_label(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|time| time.format("%b %-d, %Y, %-I:%M %p").to_string()).unwrap_or_default()
}

pub(super) struct AutomationsFeed {
    connection: Option<u64>,
    tasks: Vec<ScheduledTask>,
    error: Option<String>,
    busy_task: Option<String>,
    feed: Option<(Abort, Task<()>)>,
}

impl AutomationsFeed {
    pub fn new() -> Self {
        AutomationsFeed { connection: None, tasks: Vec::new(), error: None, busy_task: None, feed: None }
    }

    pub fn count_for(&self, thread_id: &str) -> usize {
        tasks_for_thread(&self.tasks, thread_id).len()
    }

    /// Subscribes to `scheduledTasks.subscribe` only while the details card is open and `automations`
    /// is visible; drops the subscription immediately when closed or disconnected.
    pub fn sync(&mut self, state: &Entity<AppState>, active: bool, cx: &mut Context<DetailsView>) {
        let app = state.read(cx);
        let online = matches!(app.phase, Phase::Online(ConnectionStatus::Connected));
        let connection = app.connection_id();
        if !active || !online || connection.is_none() {
            if self.feed.take().is_some() {
                self.connection = None;
                self.tasks.clear();
                self.error = None;
                self.busy_task = None;
            }
            return;
        }
        if self.feed.is_some() && self.connection == connection {
            return;
        }
        self.feed = None;
        self.connection = connection;
        self.tasks.clear();
        self.error = None;
        self.busy_task = None;

        let Some(client) = app.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Vec<ScheduledTask>, String>>();
        let driver = app.runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                let mut stream_err: Option<String> = None;
                match client.stream(method::SUBSCRIBE, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    if let Some(tasks) = decode_list(&value)
                                        && tx.send(Ok(tasks)).is_err()
                                    {
                                        return;
                                    }
                                }
                                Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                                Err(error) => {
                                    stream_err = Some(error.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                    Err(error) => stream_err = Some(error.to_string()),
                }
                if let Some(error) = stream_err
                    && tx.send(Err(error)).is_err()
                {
                    return;
                }
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((500u64 << attempt.min(4)).min(8_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(mut update) = rx.recv().await {
                while let Ok(newer) = rx.try_recv() {
                    update = newer;
                }
                let applied = this.update(cx, |view, cx| {
                    match update {
                        Ok(tasks) => {
                            if view.automations.tasks != tasks || view.automations.error.is_some() {
                                view.automations.tasks = tasks;
                                view.automations.error = None;
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            if view.automations.error.as_deref() != Some(&error) {
                                view.automations.error = Some(error);
                                cx.notify();
                            }
                        }
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.feed = Some((Abort(driver), pump));
    }

    fn run_task_rpc(&mut self, state: &Entity<AppState>, task_id: &str, rpc: &'static str, payload: serde_json::Value, cx: &mut Context<DetailsView>) {
        if self.busy_task.is_some() {
            return;
        }
        self.busy_task = Some(task_id.to_owned());
        let request = state.read(cx).rpc(rpc, payload);
        let state = state.clone();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                view.automations.busy_task = None;
                if let Err(error) = result {
                    state.update(cx, |state, cx| {
                        state.show_toast(Toast::new(ToastKind::Error, "Could not update scheduled task").description(error.to_string()), cx);
                    });
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn render(&self, state: &Entity<AppState>, thread_id: &str, separated: bool, cx: &mut Context<DetailsView>) -> Option<AnyElement> {
        let bound = tasks_for_thread(&self.tasks, thread_id);
        if self.error.is_none() && bound.is_empty() {
            return None;
        }
        let app = state.read(cx);
        let can_operate = app.client_handle().is_some_and(|client| {
            client.session_state().borrow().as_ref().is_some_and(|session| session.grants(ORCHESTRATION_OPERATE_SCOPE))
        });
        let webhook_denial = capability_denial(app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), Capability::WebhookTasks);
        let now = now_ms();
        let p = palette(cx);
        let fill = hover_fill(cx);
        let any_busy = self.busy_task.is_some();

        let manage_btn = right_panel::Button::new("details-automations-manage")
            .variant(right_panel::Variant::GhostMuted)
            .size(right_panel::Size::IconXs)
            .icon(Icon::Settings2)
            .icon_size(14.)
            .tooltip("Manage scheduled tasks")
            .on_click(|_, _, cx| crate::settings::open(t3_core::settings::search::Section::ScheduledTasks, cx))
            .into_any_element();

        let rows: Vec<AnyElement> = bound
            .into_iter()
            .map(|task| {
                let title = if task.title.trim().is_empty() { task.prompt.as_str() } else { task.title.as_str() }.to_owned();
                let subtitle = automation_schedule_line(task, once_label, now);
                let dot_color = match task.last_run_status {
                    RunStatus::Running => hsla(p.info),
                    RunStatus::Succeeded => hsla(p.terminal_running),
                    RunStatus::Failed => hsla(p.error),
                    RunStatus::Never => hsla(p.muted_foreground).opacity(0.4),
                };
                let task_id = task.id.clone();
                let run_id = task.id.clone();
                let toggle_id = task.id.clone();
                let next_enabled = !task.enabled;
                let enable_blocked = next_enabled && task.is_webhook() && webhook_denial.is_some();
                let toggle_disabled = !can_operate || any_busy || enable_blocked;
                let run_disabled = !can_operate || any_busy || task.last_run_status == RunStatus::Running;
                let toggle_tooltip = if task.enabled { "Pause automation" } else { "Enable automation" };
                let row_tip: SharedString = format!("{title}\n{subtitle}").into();

                let track_bg = if task.enabled { hsla(p.primary) } else { hsla(p.muted_foreground).opacity(0.3) };
                let thumb_offset = if task.enabled { 12. } else { 2. };
                let switch = div()
                    .id(SharedString::from(format!("details-automation-toggle-{}", task.id)))
                    .relative()
                    .w(px(26.))
                    .h(px(15.))
                    .flex_none()
                    .rounded_full()
                    .bg(track_bg)
                    .when(toggle_disabled, |this| this.opacity(0.5))
                    .when(!toggle_disabled, |this| this.cursor_pointer())
                    .tooltip(move |window, cx| right_panel::tooltip_view(toggle_tooltip.into(), window, cx))
                    .child(div().absolute().top(px(2.)).left(px(thumb_offset)).size(px(11.)).rounded_full().bg(hsla(p.background)))
                    .when(!toggle_disabled, |this| {
                        let state = state.clone();
                        this.on_click(cx.listener(move |view, _, _, cx| {
                            view.automations.run_task_rpc(&state, &toggle_id, method::SET_ENABLED, set_enabled_payload(&toggle_id, next_enabled), cx);
                        }))
                    });

                let run_btn = (!task.is_webhook()).then(|| {
                    let state = state.clone();
                    right_panel::Button::new(SharedString::from(format!("details-automation-run-{}", task_id)))
                        .variant(right_panel::Variant::GhostMuted)
                        .size(right_panel::Size::IconXs)
                        .icon(Icon::Play)
                        .icon_size(14.)
                        .disabled(run_disabled)
                        .tooltip("Run now")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.automations.run_task_rpc(&state, &run_id, method::RUN_NOW, id_payload(&run_id), cx);
                        }))
                });

                let edit_btn = right_panel::Button::new(SharedString::from(format!("details-automation-edit-{}", task.id)))
                    .variant(right_panel::Variant::GhostMuted)
                    .size(right_panel::Size::IconXs)
                    .icon(Icon::SlidersHorizontal)
                    .icon_size(14.)
                    .tooltip("Edit automation")
                    .on_click(|_, _, cx| crate::settings::open(t3_core::settings::search::Section::ScheduledTasks, cx));

                let info = v_flex()
                    .id(SharedString::from(format!("details-automation-info-{}", task.id)))
                    .flex_1()
                    .min_w_0()
                    .tooltip(move |window, cx| right_panel::tooltip_view(row_tip.clone(), window, cx))
                    .child(div().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.8)).child(title))
                    .child(div().truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(subtitle));

                let glyph = div()
                    .relative()
                    .size(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::CalendarClock.sized(px(16.)).text_color(hsla(p.muted_foreground)))
                    .child(div().absolute().top(px(-2.)).right(px(-2.)).size(px(6.)).rounded_full().bg(dot_color));

                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .hover(move |style| style.bg(fill))
                    .child(glyph)
                    .child(info)
                    .child(edit_btn)
                    .children(run_btn)
                    .child(switch)
                    .into_any_element()
            })
            .collect();

        let body = if let Some(err) = &self.error {
            div()
                .px(px(10.))
                .py(px(6.))
                .text_xs()
                .text_color(hsla(p.error))
                .child(format!("Could not load automations: {err}"))
                .into_any_element()
        } else {
            v_flex()
                .id("details-automations-list")
                .max_h(px(216.))
                .overflow_y_scroll()
                .gap(px(2.))
                .children(rows)
                .into_any_element()
        };

        Some(section_with_action(separated, Some("Automations".into()), Some(manage_btn), false, cx).child(body).into_any_element())
    }
}
