//! `ThreadAutomationsPanel` (`apps/web/src/components/chat/ThreadAutomationsPanel.tsx`):
//! scheduled tasks bound to the open thread, with status dot, schedule line,
//! Manage/Edit jump to Settings -> Scheduled Tasks, Run now, and Enable/Pause switch.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{Local, TimeZone};
use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Task, div, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_client::session::RpcError;
use t3_client::supervisor::ConnectionStatus;
use t3_core::managed_policy::{Capability, capability_denial};
use t3_core::settings::scheduled_tasks::{RunStatus, ScheduledTask, decode_list, id_payload, method, set_enabled_payload};
use t3_core::thread_details::tasks_for_thread;

use super::controls::{hover_fill, section};
use super::view::DetailsView;
use crate::app_state::{AppState, Phase};
use crate::context_menu::ThreadMenuOwner;
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
    busy: HashSet<String>,
    feed: Option<(Abort, Task<()>)>,
}

impl AutomationsFeed {
    pub fn new() -> Self {
        AutomationsFeed { connection: None, tasks: Vec::new(), busy: HashSet::new(), feed: None }
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
                self.busy.clear();
            }
            return;
        }
        if self.feed.is_some() && self.connection == connection {
            return;
        }
        self.feed = None;
        self.connection = connection;
        self.tasks.clear();
        self.busy.clear();

        let Some(client) = app.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<ScheduledTask>>();
        let driver = app.runtime().spawn(async move {
            let mut attempt = 0u32;
            loop {
                match client.stream(method::SUBSCRIBE, json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(value) => {
                                    attempt = 0;
                                    if let Some(tasks) = decode_list(&value)
                                        && tx.send(tasks).is_err()
                                    {
                                        return;
                                    }
                                }
                                Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(RpcError::Failure(failure)) if failure.tag.as_deref() == Some(AUTHORIZATION_ERROR) => return,
                    Err(_) => {}
                }
                if tx.is_closed() {
                    return;
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_millis((500u64 << attempt.min(4)).min(8_000))).await;
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(mut tasks) = rx.recv().await {
                while let Ok(newer) = rx.try_recv() {
                    tasks = newer;
                }
                let applied = this.update(cx, |view, cx| {
                    if view.automations.tasks != tasks {
                        view.automations.tasks = tasks;
                        cx.notify();
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
        if !self.busy.insert(task_id.to_owned()) {
            return;
        }
        let request = state.read(cx).rpc(rpc, payload);
        let id = task_id.to_owned();
        let state = state.clone();
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                view.automations.busy.remove(&id);
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
        if bound.is_empty() {
            return None;
        }
        let app = state.read(cx);
        let can_operate = ThreadMenuOwner::can_operate(app);
        let webhook_denial = capability_denial(app.server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), Capability::WebhookTasks);
        let now = now_ms();
        let p = palette(cx);
        let fill = hover_fill(cx);

        let rows: Vec<AnyElement> = bound
            .into_iter()
            .map(|task| {
                let busy = self.busy.contains(&task.id);
                let title = if task.title.trim().is_empty() { task.prompt.as_str() } else { task.title.as_str() }.to_owned();
                let subtitle = format!("{} \u{00b7} {}", task.schedule.label(once_label), task.state_label(now));
                let dot_color = match task.last_run_status {
                    _ if !task.enabled => hsla(p.muted_foreground).opacity(0.35),
                    RunStatus::Running => hsla(p.primary),
                    RunStatus::Failed => hsla(p.error),
                    RunStatus::Succeeded => hsla(p.terminal_running),
                    RunStatus::Never => hsla(p.muted_foreground).opacity(0.55),
                };
                let task_id = task.id.clone();
                let run_id = task.id.clone();
                let toggle_id = task.id.clone();
                let next_enabled = !task.enabled;
                let enable_blocked = next_enabled && task.is_webhook() && webhook_denial.is_some();
                let error_text = task.last_run_error.clone();
                let toggle_tooltip = if task.enabled { "Pause automation" } else { "Enable automation" };

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
                    .when(!can_operate || busy || enable_blocked, |this| this.opacity(0.5))
                    .when(can_operate && !busy && !enable_blocked, |this| this.cursor_pointer())
                    .tooltip(move |window, cx| right_panel::tooltip_view(toggle_tooltip.into(), window, cx))
                    .child(div().absolute().top(px(2.)).left(px(thumb_offset)).size(px(11.)).rounded_full().bg(hsla(p.background)))
                    .when(can_operate && !busy && !enable_blocked, |this| {
                        let state = state.clone();
                        this.on_click(cx.listener(move |view, _, _, cx| {
                            view.automations.run_task_rpc(&state, &toggle_id, method::SET_ENABLED, set_enabled_payload(&toggle_id, next_enabled), cx);
                        }))
                    });

                let run_btn = (!task.is_webhook()).then(|| {
                    let state = state.clone();
                    right_panel::Button::new(SharedString::from(format!("details-automation-run-{}", task_id)))
                        .icon(Icon::Play)
                        .icon_size(12.)
                        .disabled(!can_operate || busy)
                        .tooltip("Run now")
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.automations.run_task_rpc(&state, &run_id, method::RUN_NOW, id_payload(&run_id), cx);
                        }))
                });

                let edit_btn = right_panel::Button::new(SharedString::from(format!("details-automation-edit-{}", task.id)))
                    .icon(Icon::SlidersHorizontal)
                    .icon_size(12.)
                    .tooltip("Edit automation")
                    .on_click(|_, _, cx| crate::settings::open(t3_core::settings::search::Section::ScheduledTasks, cx));

                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .hover(move |style| style.bg(fill))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(7.)).flex_none().rounded_full().bg(dot_color))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground).opacity(0.85)).child(title))
                            .child(edit_btn)
                            .children(run_btn)
                            .child(switch),
                    )
                    .child(div().pl(px(13.)).truncate().text_size(px(11.)).text_color(hsla(p.muted_foreground)).child(subtitle))
                    .children(error_text.map(|err| {
                        div().pl(px(13.)).truncate().text_size(px(11.)).text_color(hsla(p.error)).child(err)
                    }))
                    .into_any_element()
            })
            .collect();

        Some(section(separated, Some("Automations".into()), cx).child(v_flex().gap(px(2.)).children(rows)).into_any_element())
    }
}
