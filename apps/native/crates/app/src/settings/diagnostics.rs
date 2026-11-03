//! Diagnostics: `DiagnosticsSettingsPanel` in `web/src/components/settings/DiagnosticsSettings.tsx`
//! for the connected environment (live processes with INT and KILL, resource history, trace
//! diagnostics and its tables, the logs folder) and `TelemetryExportSettings.tsx`. The resource
//! monitor ports `ResourceTelemetryDiagnostics.tsx`. Rules and formatting live in
//! `t3_core::settings::diagnostics`; process tables are virtualized.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, prelude::FluentBuilder, relative, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::json;
use t3_client::supervisor::ConnectionStatus;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::device::SETTINGS_WRITE_SCOPE;
use t3_core::project_search::{OPEN_IN_EDITOR, open_in_editor_request, preferred_editor};
use t3_core::settings::ServerSettingChange;
use t3_core::settings::diagnostics::{
    self as model, Dot, EndpointCheck, HISTORY_WINDOWS, OtlpSignal, ProcessDiagnostics, Query, ResourceHistory, Signal, SignalOutcome, TraceDiagnostics,
};
use t3_core::settings::maintenance::{format_bytes, format_count};
use t3_core::usage::{ConnectionPhase, resolve_usage_access};

use super::controls::{Button, ButtonSize, ButtonVariant, InlineButton, InlineTone, group, icon, r, section_title, tooltip};
use super::providers::failure_text;
use super::{Block, SettingsView, store};
use crate::app_state::Phase;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

pub(crate) mod resource_monitor;

const PROCESS_ROW: f32 = 36.;
/// `max-h-[min(64vh,44rem)]`.
const TABLE_MAX: f32 = 704.;

struct Telemetry {
    inputs: [Entity<InputState>; 3],
    /// Fields that differ from what is saved; the rest follow the server.
    drafts: HashMap<OtlpSignal, String>,
    saving: bool,
    message: Option<&'static str>,
    checks: HashMap<OtlpSignal, EndpointCheck>,
    /// Seeded once per visit from what runs or is saved, then sent once checks are allowed.
    seeded: bool,
    sent: bool,
    /// Fields a save refused (`ENDPOINT_HINT`), until edited.
    invalid: Vec<OtlpSignal>,
}

pub(crate) struct DiagnosticsState {
    monitor: resource_monitor::MonitorState,
    processes: Query<ProcessDiagnostics>,
    history: Query<ResourceHistory>,
    trace: Query<TraceDiagnostics>,
    /// The connection the queries were sent on.
    loaded_for: Option<u64>,
    window: usize,
    collapsed: HashSet<u64>,
    signaling: Option<u64>,
    opening_logs: bool,
    open_logs_error: Option<String>,
    expanded: HashSet<String>,
    copied: Option<String>,
    /// The export form shows once the first tables are in, so a search jump lands steadily.
    form_ready: bool,
    pending_scroll: bool,
    telemetry: Telemetry,
    ticker: Option<Task<()>>,
    grants: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DiagnosticsState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let inputs = OtlpSignal::ALL.map(|signal| cx.new(|cx| InputState::new(window, cx).placeholder(signal.placeholder())));
        let subscriptions = OtlpSignal::ALL
            .iter()
            .zip(inputs.iter())
            .map(|(signal, input)| {
                let signal = *signal;
                cx.subscribe_in(input, window, move |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.telemetry_edited(signal, cx),
                    InputEvent::PressEnter { .. } => this.save_telemetry(cx),
                    _ => {}
                })
            })
            .collect();
        DiagnosticsState {
            monitor: resource_monitor::MonitorState::default(),
            processes: Query::default(),
            history: Query::default(),
            trace: Query::default(),
            loaded_for: None,
            window: model::DEFAULT_HISTORY_WINDOW,
            collapsed: HashSet::new(),
            signaling: None,
            opening_logs: false,
            open_logs_error: None,
            expanded: HashSet::new(),
            copied: None,
            form_ready: false,
            pending_scroll: false,
            telemetry: Telemetry { inputs, drafts: HashMap::new(), saving: false, message: None, checks: HashMap::new(), seeded: false, sent: false, invalid: Vec::new() },
            ticker: None,
            grants: None,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        self.telemetry.inputs.iter()
    }

    /// A fresh visit: signals and pending state belong to one visit (`key={environmentId}`).
    pub(super) fn reset(&mut self) {
        self.leave();
        self.processes.reset();
        self.history.reset();
        self.trace.reset();
        self.window = model::DEFAULT_HISTORY_WINDOW;
        self.collapsed.clear();
        self.signaling = None;
        self.opening_logs = false;
        self.open_logs_error = None;
        self.expanded.clear();
        self.form_ready = false;
        self.telemetry.drafts.clear();
        self.telemetry.message = None;
        self.telemetry.checks.clear();
        self.telemetry.seeded = false;
        self.telemetry.sent = false;
        self.telemetry.invalid.clear();
    }

    /// Off the page: clocks and watchers stop, and nothing reloads until it shows again.
    pub(super) fn leave(&mut self) {
        self.monitor.leave();
        self.loaded_for = None;
        self.ticker = None;
        self.grants = None;
    }
}

fn now_ms() -> i64 {
    crate::sidebar::controller::now_ms()
}

/// `max-h-[min(64vh,44rem)]` for `rows` rows of `PROCESS_ROW`.
fn table_height(rows: usize, window: &Window) -> f32 {
    let viewport = f32::from(window.viewport_size().height) * 0.64;
    (rows.max(1) as f32 * PROCESS_ROW).min(viewport.min(TABLE_MAX))
}

impl SettingsView {
    fn connection_phase(&self, cx: &App) -> ConnectionPhase {
        match &self.state.read(cx).phase {
            Phase::Online(ConnectionStatus::Connected) => ConnectionPhase::Connected,
            Phase::Online(ConnectionStatus::Connecting) => ConnectionPhase::Connecting,
            Phase::Online(ConnectionStatus::Reconnecting { .. }) => ConnectionPhase::Reconnecting,
            _ => ConnectionPhase::Disconnected,
        }
    }

    /// Repaints when this device's grants change, and keeps relative times current.
    fn watch_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.diagnostics.grants.is_none()
            && let Some(client) = self.state.read(cx).client_handle()
        {
            let mut grants = client.session_state();
            self.diagnostics.grants = Some(cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
        if self.diagnostics.ticker.is_none() {
            self.diagnostics.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(30)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    fn load_processes(&mut self, cx: &mut Context<Self>) {
        let request = self.diagnostics.processes.begin();
        let task = self
            .state
            .read(cx)
            .request(model::GET_PROCESS_DIAGNOSTICS, json!({}), |reply| model::decode_process_diagnostics(&reply).ok_or_else(|| "Could not read process diagnostics.".to_owned()));
        self.finish_query(task, request, |this| &mut this.diagnostics.processes, cx);
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let request = self.diagnostics.history.begin();
        let payload = HISTORY_WINDOWS[self.diagnostics.window].payload();
        let task = self
            .state
            .read(cx)
            .request(model::GET_PROCESS_RESOURCE_HISTORY, payload, |reply| model::decode_resource_history(&reply).ok_or_else(|| "Could not read resource history.".to_owned()));
        self.finish_query(task, request, |this| &mut this.diagnostics.history, cx);
    }

    fn load_trace(&mut self, cx: &mut Context<Self>) {
        let request = self.diagnostics.trace.begin();
        let task = self
            .state
            .read(cx)
            .request(model::GET_TRACE_DIAGNOSTICS, json!({}), |reply| model::decode_trace_diagnostics(&reply).ok_or_else(|| "Could not read trace diagnostics.".to_owned()));
        self.finish_query(task, request, |this| &mut this.diagnostics.trace, cx);
    }

    fn finish_query<T: Send + 'static>(
        &mut self,
        task: Option<tokio::task::JoinHandle<Result<T, String>>>,
        request: u64,
        slot: fn(&mut SettingsView) -> &mut Query<T>,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = task else {
            slot(self).finish(request, Err("The connection closed. Try again once it reconnects.".to_owned()));
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if slot(this).finish(request, result) {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// `signalProcess`: SIGKILL asks first; the server refuses a pid whose start time moved on, so
    /// a process that exited meanwhile is never signaled by mistake.
    fn request_signal(&mut self, index: usize, signal: Signal, cx: &mut Context<Self>) {
        if self.diagnostics.signaling.is_some() || !self.may(model::ENVIRONMENT_MAINTAIN_SCOPE, cx) {
            return;
        }
        let Some(process) = self.diagnostics.processes.data.as_ref().and_then(|data| data.processes.get(index)).cloned() else {
            return;
        };
        if signal == Signal::Kill {
            // The confirmation is modal, so the row needs no lock while it is open.
            let (title, description) = model::kill_confirmation(process.pid);
            let view = cx.entity().downgrade();
            crate::dialog::ask(
                title,
                description,
                "Confirm",
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if this.diagnostics.signaling.is_none() {
                            this.send_signal(process, signal, cx);
                        }
                    });
                },
                cx,
            );
            return;
        }
        self.send_signal(process, signal, cx);
    }

    fn send_signal(&mut self, process: model::ProcessEntry, signal: Signal, cx: &mut Context<Self>) {
        if !self.may(model::ENVIRONMENT_MAINTAIN_SCOPE, cx) {
            self.diagnostics.signaling = None;
            return;
        }
        self.diagnostics.signaling = Some(process.pid);
        let task = self.state.read(cx).rpc(model::SIGNAL_PROCESS, signal.payload(&process));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.diagnostics.signaling = None;
                // The server answered: the table refreshes whatever it said.
                let (toast, refresh) = match result {
                    Err(error) => (
                        Some(Toast::new(ToastKind::Error, model::signal_failure_title(signal)).description(failure_text(&error, &format!("Failed to send {}.", signal.as_str())))),
                        false,
                    ),
                    Ok(reply) => match model::signal_outcome(&reply, signal) {
                        SignalOutcome::Signaled => (None, true),
                        SignalOutcome::AlreadyExited => (Some(Toast::new(ToastKind::Info, model::ALREADY_EXITED_TITLE).description(model::ALREADY_EXITED_DESCRIPTION)), true),
                        SignalOutcome::Failed(message) => (Some(Toast::new(ToastKind::Error, model::signal_failure_title(signal)).description(message)), true),
                    },
                };
                if let Some(toast) = toast {
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
                if refresh {
                    this.load_processes(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_logs_directory(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.thread_settings(cx) else {
            return;
        };
        let Some(path) = config.observability.as_ref().and_then(|observability| observability.logs_directory_path.clone()) else {
            return;
        };
        if !self.may(ORCHESTRATION_OPERATE_SCOPE, cx) {
            return;
        }
        let Some((editor, _)) = preferred_editor(&config.available_editors, None) else {
            self.diagnostics.open_logs_error = Some("Unable to open logs folder.".to_owned());
            return cx.notify();
        };
        self.diagnostics.opening_logs = true;
        self.diagnostics.open_logs_error = None;
        let task = self.state.read(cx).rpc(OPEN_IN_EDITOR, open_in_editor_request(&path, editor));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.diagnostics.opening_logs = false;
                if let Err(error) = result {
                    this.diagnostics.open_logs_error = Some(failure_text(&error, "Unable to open logs folder."));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn copy_trace_id(&mut self, trace_id: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(trace_id.clone()));
        self.diagnostics.copied = Some(trace_id.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1200)).await;
            let _ = this.update(cx, |this, cx| {
                if this.diagnostics.copied.as_deref() == Some(trace_id.as_str()) {
                    this.diagnostics.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn diagnostics_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let session = self.session(cx);
        let access = resolve_usage_access(self.connection_phase(cx), session.as_ref(), false);
        let connection = self.state.read(cx).connection_id();
        if !access.can_read_diagnostics || connection.is_none() {
            let text = if connection.is_none() {
                "Connect an environment to see diagnostics."
            } else if access.is_pending {
                "Checking diagnostics access\u{2026}"
            } else {
                access.error.unwrap_or_default()
            };
            self.diagnostics.monitor.leave();
            self.watch_diagnostics(cx);
            return vec![Block::new(&[], div().px(r(16.)).text_sm().text_color(hsla(p.muted_foreground)).child(text))];
        }
        self.watch_diagnostics(cx);
        if self.diagnostics.loaded_for != connection {
            self.diagnostics.loaded_for = connection;
            self.load_processes(cx);
            self.load_history(cx);
            self.load_trace(cx);
        }
        let initial = self.diagnostics.processes.initial_loading() || self.diagnostics.history.initial_loading() || self.diagnostics.trace.initial_loading();
        if !initial && !self.diagnostics.form_ready {
            self.diagnostics.form_ready = true;
            if self.diagnostics.pending_scroll {
                self.diagnostics.pending_scroll = false;
                self.scroll_to_target = true;
            }
        }
        if !self.diagnostics.form_ready && self.scroll_to_target && self.is_target("telemetry-export") {
            self.diagnostics.pending_scroll = true;
            self.scroll_to_target = false;
        }
        self.watch_resource_monitor(cx);
        let now = now_ms();
        let mut blocks =
            vec![Block::new(&[], self.live_processes(window, now, cx)), Block::new(&[], self.resource_history(window, now, cx)), Block::new(&[], self.trace_section(now, cx))];
        blocks.splice(0..0, self.resource_monitor_blocks(window, cx));
        blocks.extend(self.trace_tables(now, cx));
        if self.diagnostics.form_ready {
            blocks.push(Block::new(&["telemetry-export"], self.telemetry_form(window, cx)));
        }
        blocks
    }

    /// A section with a title and actions at its end (`SettingsSection` with `headerAction`).
    fn diagnostics_section(&self, title: &'static str, actions: Option<AnyElement>, content: Vec<AnyElement>, cx: &App) -> AnyElement {
        v_flex()
            .gap(r(10.))
            .child(h_flex().justify_between().gap(r(16.)).child(section_title(title)).children(actions.map(|actions| div().pr(r(16.)).child(actions))))
            .child(group(content, cx))
            .into_any_element()
    }

    fn last_checked(read_at: Option<&str>, now: i64, cx: &App) -> AnyElement {
        let p = palette(cx);
        let alpha = if read_at.is_some() { 0.6 } else { 0.5 };
        div().text_size(r(11.)).text_color(hsla(p.muted_foreground).opacity(alpha)).child(model::last_checked(read_at, now)).into_any_element()
    }

    fn refresh_button(id: &'static str, label: &'static str, pending: bool, on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> Button {
        Button::new(id, ButtonVariant::GhostMuted, ButtonSize::IconXs)
            .icon(if pending { Icon::LoaderCircle } else { Icon::RefreshCw })
            .tooltip(label)
            .disabled(pending)
            .on_click(on_click)
    }

    /// `StatsGrid` of four `StatBlock`s.
    fn stats(blocks: Vec<(&'static str, String, Option<String>, Option<gpui::Hsla>)>, cx: &App) -> AnyElement {
        let p = palette(cx);
        let divider = hsla(p.border).opacity(0.6);
        h_flex()
            .children(blocks.into_iter().enumerate().map(|(index, (label, value, tip, tone))| {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .px(r(20.))
                    .py(r(12.))
                    .when(index > 0, |this| this.border_l_1().border_color(divider))
                    .child(
                        h_flex()
                            .gap(r(6.))
                            .text_size(r(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.muted_foreground).opacity(0.7))
                            .child(div().truncate().child(label.to_uppercase()))
                            .children(tip.map(|tip| {
                                div()
                                    .id(SharedString::from(format!("stat-{label}")))
                                    .child(icon(Icon::Info, 12., hsla(p.muted_foreground).opacity(0.6)))
                                    .tooltip(tooltip(tip.into()))
                            })),
                    )
                    .child(
                        div()
                            .mt(r(4.))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tone.unwrap_or(hsla(p.foreground)))
                            .child(value),
                    )
            }))
            .into_any_element()
    }

    fn error_lines(lines: Vec<(String, gpui::Hsla)>, cx: &App) -> Option<AnyElement> {
        if lines.is_empty() {
            return None;
        }
        let p = palette(cx);
        Some(
            v_flex()
                .gap(r(8.))
                .px(r(20.))
                .py(r(12.))
                .border_t_1()
                .border_color(hsla(p.border).opacity(0.6))
                .text_xs()
                .children(lines.into_iter().map(|(text, color)| {
                    h_flex().items_start().gap(r(8.)).text_color(color).child(div().pt(r(1.)).child(icon(Icon::TriangleAlert, 14., color))).child(div().min_w_0().child(text))
                }))
                .into_any_element(),
        )
    }

    fn live_processes(&mut self, window: &Window, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = &self.diagnostics.processes;
        let data = query.data.as_ref();
        let dots = |value: Option<String>| value.unwrap_or_else(|| "...".to_owned());
        let stats = Self::stats(
            vec![
                ("Child Processes", dots(data.map(|data| format_count(data.process_count))), None, None),
                (
                    "CPU",
                    dots(data.map(|data| model::percent(data.total_cpu_percent))),
                    Some("Total CPU across live child processes of the current server process. The desktop shell and other parent processes are not included.".to_owned()),
                    None,
                ),
                (
                    "Memory",
                    dots(data.map(|data| format_bytes(data.total_rss_bytes))),
                    Some(
                        "Total resident memory across live child processes of the current server process. The desktop shell and other parent processes are not included."
                            .to_owned(),
                    ),
                    None,
                ),
                ("Server PID", dots(data.map(|data| data.server_pid.to_string())), None, None),
            ],
            cx,
        );
        let destructive = hsla(p.destructive_foreground);
        let errors = Self::error_lines(data.and_then(|data| data.error.clone()).into_iter().chain(query.error.clone()).map(|text| (text, destructive)).collect(), cx);
        let initial = query.initial_loading();
        let pending = query.pending;
        let read_at = data.map(|data| data.read_at.clone());
        let processes = data.map(|data| data.processes.clone()).unwrap_or_default();
        let visible = model::visible_process_tree(&processes, &self.diagnostics.collapsed);
        let can_maintain = self.may(model::ENVIRONMENT_MAINTAIN_SCOPE, cx);
        let table = self.process_table(processes, visible, can_maintain, initial, window, cx);
        let actions = h_flex()
            .gap(r(6.))
            .child(Self::last_checked(read_at.as_deref(), now, cx))
            .child(Self::refresh_button("refresh-processes", "Refresh process diagnostics", pending, cx.listener(|this, _, _, cx| this.load_processes(cx))))
            .into_any_element();
        let mut content = vec![stats];
        content.extend(errors);
        content.push(table);
        self.diagnostics_section("Live Processes", Some(actions), vec![v_flex().children(content).into_any_element()], cx)
    }

    fn header_cell(label: &'static str, width: f32, end: bool, cx: &App) -> gpui::Div {
        let p = palette(cx);
        div()
            .w(relative(width))
            .px(r(12.))
            .py(r(8.))
            .when(end, |this| this.flex().justify_end())
            .text_size(r(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(hsla(p.muted_foreground).opacity(0.7))
            .child(label.to_uppercase())
    }

    fn cell(width: f32, end: bool) -> gpui::Div {
        div().w(relative(width)).min_w_0().px(r(12.)).when(end, |this| this.flex().justify_end())
    }

    /// `ProcessDiagnosticsTable`: a virtualized tree with collapsible children and signals.
    fn process_table(
        &mut self,
        processes: Vec<model::ProcessEntry>,
        visible: Vec<usize>,
        can_maintain: bool,
        initial: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let widths = [0.24, 0.08, 0.10, 0.33, 0.08, 0.11, 0.06];
        let header = h_flex()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card))
            .child(Self::header_cell("Name", widths[0], false, cx).pl(r(20.)))
            .child(Self::header_cell("CPU", widths[1], true, cx))
            .child(Self::header_cell("Memory", widths[2], true, cx))
            .child(Self::header_cell("Command", widths[3], false, cx))
            .child(Self::header_cell("PID", widths[4], true, cx))
            .child(Self::header_cell("Type", widths[5], false, cx))
            .child(Self::header_cell("Kill", widths[6], true, cx).pr(r(16.)));
        let body = if visible.is_empty() {
            div()
                .px(r(20.))
                .py(r(16.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(if initial { "Loading live processes..." } else { "No live descendant processes found." })
                .into_any_element()
        } else {
            let height = table_height(visible.len(), window);
            let processes = std::rc::Rc::new(processes);
            let visible = std::rc::Rc::new(visible);
            uniform_list(
                "diagnostics-processes",
                visible.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range.filter_map(|row| visible.get(row).map(|index| this.process_row(*index, &processes[*index], can_maintain, widths, cx))).collect::<Vec<_>>()
                }),
            )
            .h(r(height))
            .into_any_element()
        };
        div()
            .id("diagnostics-process-table")
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .overflow_x_scroll()
            .child(v_flex().min_w(r(1040.)).text_xs().child(header).child(body))
            .into_any_element()
    }

    fn process_row(&self, index: usize, process: &model::ProcessEntry, can_maintain: bool, widths: [f32; 7], cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let name = model::process_name(&process.command);
        let pid = process.pid;
        let expanded = !self.diagnostics.collapsed.contains(&pid);
        let toggle = if process.has_children {
            Button::new(SharedString::from(format!("process-toggle-{pid}")), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                .icon(if expanded { Icon::ChevronDown } else { Icon::ChevronRight })
                .tooltip(if expanded { format!("Collapse {name}") } else { format!("Expand {name}") })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.diagnostics.collapsed.remove(&pid) {
                        this.diagnostics.collapsed.insert(pid);
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().size(r(20.)).flex_none().into_any_element()
        };
        let disabled = !can_maintain || self.diagnostics.signaling == Some(pid);
        let command = SharedString::from(process.command.clone());
        h_flex()
            .id(SharedString::from(format!("process-row-{pid}")))
            .h(r(PROCESS_ROW))
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.5))
            .hover(|this| this.bg(hsla(p.muted).opacity(0.2)))
            .child(
                Self::cell(widths[0], false).pl(r(20.)).child(
                    h_flex()
                        .gap(r(8.))
                        .pl(r(process.depth.min(6) as f32 * 10.))
                        .child(toggle)
                        .child(div().size(r(6.)).flex_none().rounded_full().bg(hsla(p.success).opacity(0.8)))
                        .child(
                            div()
                                .id(SharedString::from(format!("process-name-{pid}")))
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child(name)
                                .tooltip(tooltip(command.clone())),
                        ),
                ),
            )
            .child(Self::cell(widths[1], true).font_family(mono.clone()).child(model::percent(process.cpu_percent)))
            .child(Self::cell(widths[2], true).font_family(mono.clone()).child(format_bytes(process.rss_bytes)))
            .child(Self::cell(widths[3], false).child(
                div().id(SharedString::from(format!("process-command-{pid}"))).truncate().text_color(hsla(p.muted_foreground)).child(command.clone()).tooltip(tooltip(command)),
            ))
            .child(Self::cell(widths[4], true).font_family(mono).text_color(hsla(p.muted_foreground)).child(pid.to_string()))
            .child(Self::cell(widths[5], false).truncate().text_color(hsla(p.muted_foreground)).child(model::process_type(process)))
            .child(
                Self::cell(widths[6], true).pr(r(16.)).child(
                    h_flex()
                        .gap(r(6.))
                        .child(
                            InlineButton::new(SharedString::from(format!("process-int-{pid}")), "INT", InlineTone::Muted)
                                .tooltip("Send SIGINT")
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| this.request_signal(index, Signal::Int, cx))),
                        )
                        .child(
                            InlineButton::new(SharedString::from(format!("process-kill-{pid}")), "KILL", InlineTone::Destructive)
                                .tooltip("Send SIGKILL")
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| this.request_signal(index, Signal::Kill, cx))),
                        ),
                ),
            )
            .into_any_element()
    }

    fn resource_history(&mut self, window: &Window, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = &self.diagnostics.history;
        let data = query.data.as_ref();
        let dots = |value: Option<String>| value.unwrap_or_else(|| "...".to_owned());
        let stats = Self::stats(
            vec![
                (
                    "CPU Time",
                    dots(data.map(|data| model::format_cpu_time(data.total_cpu_seconds_approx))),
                    Some("Approximate active CPU time for the T3 server root process and its descendants during the selected window. It grows only while sampled processes use CPU and older samples leave as the window moves.".to_owned()),
                    None,
                ),
                (
                    "Samples",
                    dots(data.map(|data| format_count(data.retained_sample_count))),
                    Some("In-memory process samples retained by the server. The server samples only while diagnostics is open, and this resets when the server restarts.".to_owned()),
                    None,
                ),
                ("Interval", dots(data.map(ResourceHistory::interval_label)), None, None),
                ("Processes", dots(data.map(|data| format_count(data.top_processes.len() as f64))), None, None),
            ],
            cx,
        );
        let destructive = hsla(p.destructive_foreground);
        let errors = Self::error_lines(data.and_then(|data| data.error.clone()).into_iter().chain(query.error.clone()).map(|text| (text, destructive)).collect(), cx);
        let chart = self.history_chart(data, cx);
        let initial = query.initial_loading();
        let pending = query.pending;
        let read_at = data.map(|data| data.read_at.clone());
        let table = self.history_table(window, initial, cx);
        let selected = self.diagnostics.window;
        let windows = h_flex().gap(r(2.)).p(r(2.)).rounded(r(8.)).bg(hsla(p.input).opacity(0.4)).children(HISTORY_WINDOWS.iter().enumerate().map(|(index, option)| {
            let active = index == selected;
            div()
                .id(("history-window", index))
                .h(r(24.))
                .px(r(10.))
                .flex()
                .items_center()
                .rounded(r(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .text_color(if active { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                .when(active, |this| this.bg(hsla(p.background)))
                .when(!active, |this| this.hover(|this| this.bg(hsla(p.background).opacity(0.55))))
                .child(option.label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.diagnostics.window != index {
                        this.diagnostics.window = index;
                        this.load_history(cx);
                    }
                }))
        }));
        let actions = h_flex()
            .gap(r(6.))
            .child(windows)
            .child(Self::last_checked(read_at.as_deref(), now, cx))
            .child(Self::refresh_button("refresh-history", "Refresh resource history", pending, cx.listener(|this, _, _, cx| this.load_history(cx))))
            .into_any_element();
        let mut content = vec![stats];
        content.extend(errors);
        content.push(chart);
        content.push(table);
        self.diagnostics_section("Resource History", Some(actions), vec![v_flex().children(content).into_any_element()], cx)
    }

    /// `ProcessResourceHistoryChart`: per bucket, the peak behind the average.
    fn history_chart(&self, data: Option<&ResourceHistory>, cx: &App) -> AnyElement {
        let p = palette(cx);
        let bars = data.map(ResourceHistory::bars).unwrap_or_default();
        let buckets = data.map(|data| data.buckets.clone()).unwrap_or_default();
        div()
            .px(r(20.))
            .py(r(12.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .child(h_flex().h(r(112.)).items_end().gap(r(4.)).p(r(8.)).rounded(r(4.)).overflow_hidden().bg(hsla(p.muted).opacity(0.1)).children(
                bars.into_iter().zip(buckets).enumerate().map(|(index, ((peak, average), bucket))| {
                    div()
                        .id(("history-bar", index))
                        .relative()
                        .flex_1()
                        .min_w(r(4.))
                        .h_full()
                        .child(div().absolute().left_0().right_0().bottom_0().h(relative(peak as f32 / 100.)).rounded_t(r(2.)).bg(hsla(p.foreground).opacity(0.15)))
                        .child(div().absolute().left_0().right_0().bottom_0().h(relative(average as f32 / 100.)).rounded_t(r(2.)).bg(hsla(p.foreground).opacity(0.6)))
                        .tooltip(tooltip(format!("Avg {}, peak {}", model::percent(bucket.avg_cpu_percent), model::percent(bucket.max_cpu_percent)).into()))
                }),
            ))
            .into_any_element()
    }

    /// `ProcessResourceHistoryTable`.
    fn history_table(&mut self, window: &Window, initial: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let widths = [0.24, 0.10, 0.10, 0.10, 0.10, 0.10, 0.16, 0.10];
        let header = h_flex()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card))
            .child(Self::header_cell("Process", widths[0], false, cx).pl(r(20.)))
            .child(Self::header_cell("CPU Time", widths[1], true, cx))
            .child(Self::header_cell("Current", widths[2], true, cx))
            .child(Self::header_cell("Average", widths[3], true, cx))
            .child(Self::header_cell("Peak", widths[4], true, cx))
            .child(Self::header_cell("Max Mem", widths[5], true, cx))
            .child(Self::header_cell("Command", widths[6], false, cx))
            .child(Self::header_cell("PID", widths[7], true, cx).pr(r(20.)));
        let count = self.diagnostics.history.data.as_ref().map_or(0, |data| data.top_processes.len());
        let body = if count == 0 {
            div()
                .px(r(20.))
                .py(r(16.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(if initial { "Collecting process resource samples..." } else { "No process resource samples found for this window." })
                .into_any_element()
        } else {
            uniform_list(
                "diagnostics-history",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| range.filter_map(|index| this.history_row(index, widths, cx)).collect::<Vec<_>>()),
            )
            .h(r(table_height(count, window)))
            .into_any_element()
        };
        div()
            .id("diagnostics-history-table")
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .overflow_x_scroll()
            .child(v_flex().min_w(r(980.)).text_xs().child(header).child(body))
            .into_any_element()
    }

    fn history_row(&self, index: usize, widths: [f32; 8], cx: &App) -> Option<AnyElement> {
        let p = palette(cx);
        let data = self.diagnostics.history.data.as_ref()?;
        let process = data.top_processes.get(index)?;
        let mono = cx.theme().mono_font_family.clone();
        let command = SharedString::from(process.command.clone());
        let number = |width: f32, text: String| Self::cell(width, true).font_family(mono.clone()).child(text);
        let dot = if process.is_server_root { hsla(p.warning).opacity(0.9) } else { hsla(p.success).opacity(0.8) };
        Some(
            h_flex()
                .id(SharedString::from(format!("history-row-{}", process.process_key)))
                .h(r(PROCESS_ROW))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.5))
                .hover(|this| this.bg(hsla(p.muted).opacity(0.2)))
                .child(
                    Self::cell(widths[0], false).pl(r(20.)).child(
                        h_flex()
                            .gap(r(8.))
                            .pl(r(data.visual_depth(process).min(6) as f32 * 10.))
                            .child(div().size(r(20.)).flex_none())
                            .child(div().size(r(6.)).flex_none().rounded_full().bg(dot))
                            .child(
                                div()
                                    .id(SharedString::from(format!("history-name-{}", process.process_key)))
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(model::short_process_name(&process.command))
                                    .tooltip(tooltip(command.clone())),
                            ),
                    ),
                )
                .child(number(widths[1], model::format_cpu_time(process.cpu_seconds_approx)))
                .child(number(widths[2], model::percent(process.current_cpu_percent)))
                .child(number(widths[3], model::percent(process.avg_cpu_percent)))
                .child(number(widths[4], model::percent(process.max_cpu_percent)))
                .child(number(widths[5], format_bytes(process.max_rss_bytes)))
                .child(
                    Self::cell(widths[6], false).child(
                        div()
                            .id(SharedString::from(format!("history-command-{}", process.process_key)))
                            .truncate()
                            .text_color(hsla(p.muted_foreground))
                            .child(command.clone())
                            .tooltip(tooltip(command)),
                    ),
                )
                .child(Self::cell(widths[7], true).pr(r(20.)).font_family(mono).text_color(hsla(p.muted_foreground)).child(process.pid.to_string()))
                .into_any_element(),
        )
    }

    fn trace_section(&mut self, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = &self.diagnostics.trace;
        let data = query.data.as_ref();
        let dots = |value: Option<String>| value.unwrap_or_else(|| "...".to_owned());
        let danger = hsla(p.destructive_foreground);
        let warning = hsla(p.warning_foreground);
        let tone = |count: Option<f64>, color| count.filter(|count| *count > 0.).map(|_| color);
        let stats = Self::stats(
            vec![
                ("Spans", dots(data.map(|data| format_count(data.record_count))), None, None),
                ("Failures", dots(data.map(|data| format_count(data.failure_count))), None, tone(data.map(|data| data.failure_count), danger)),
                (
                    "Slow Spans",
                    dots(data.map(|data| format_count(data.slow_span_count))),
                    Some(data.map_or_else(|| "Spans at or above the configured slow-span threshold.".to_owned(), TraceDiagnostics::slow_span_tooltip)),
                    tone(data.map(|data| data.slow_span_count), warning),
                ),
                ("Parse Errors", dots(data.map(|data| format_count(data.parse_error_count))), None, tone(data.map(|data| data.parse_error_count), warning)),
            ],
            cx,
        );
        let mut lines = Vec::new();
        lines.extend(self.diagnostics.open_logs_error.clone().map(|text| (text, danger)));
        if let Some(data) = data
            && let Some(text) = data.error_text()
        {
            lines.push((text, if data.partial_failure { warning } else { danger }));
        }
        lines.extend(query.error.clone().map(|text| (text, danger)));
        let errors = Self::error_lines(lines, cx);
        let pending = query.pending;
        let read_at = data.map(|data| data.read_at.clone());
        let has_logs = self.thread_settings(cx).and_then(|config| config.observability.as_ref()).is_some_and(|observability| observability.logs_directory_path.is_some());
        let can_open = self.may(ORCHESTRATION_OPERATE_SCOPE, cx) && has_logs && !self.diagnostics.opening_logs;
        let actions = h_flex()
            .gap(r(6.))
            .child(Self::last_checked(read_at.as_deref(), now, cx))
            .child(
                Button::new("open-logs-folder", ButtonVariant::GhostMuted, ButtonSize::IconXs)
                    .icon(Icon::FolderOpen)
                    .tooltip("Open logs folder")
                    .disabled(!can_open)
                    .on_click(cx.listener(|this, _, _, cx| this.open_logs_directory(cx))),
            )
            .child(Self::refresh_button("refresh-trace", "Refresh trace diagnostics", pending, cx.listener(|this, _, _, cx| this.load_trace(cx))))
            .into_any_element();
        let mut content = vec![stats];
        content.extend(errors);
        self.diagnostics_section("Trace Diagnostics", Some(actions), vec![v_flex().children(content).into_any_element()], cx)
    }

    /// `ExpandableText`: long or multi-line text clamps until expanded.
    fn expandable(&self, key: String, text: &str, lines: usize, expand_label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let can_expand = model::can_expand(text);
        let expanded = self.diagnostics.expanded.contains(&key);
        let toggle_key = key.clone();
        v_flex()
            .min_w_0()
            // `line-clamp`: GPUI clamps each paragraph alone, so the box clips at that many lines.
            .child(div().line_height(r(16.)).when(can_expand && !expanded, |this| this.max_h(r(16. * lines as f32)).overflow_hidden()).child(text.to_owned()))
            .when(can_expand, |this| {
                this.child(div().mt(r(4.)).child(
                    InlineButton::new(SharedString::from(format!("expand-{key}")), if expanded { "Show less" } else { expand_label }, InlineTone::Muted).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if !this.diagnostics.expanded.remove(&toggle_key) {
                                this.diagnostics.expanded.insert(toggle_key.clone());
                            }
                            cx.notify();
                        },
                    )),
                ))
            })
            .into_any_element()
    }

    /// `TraceIdCell`: the shortened id, its full form on hover, and a copy button.
    fn trace_id_cell(&self, key: &str, trace_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let copied = self.diagnostics.copied.as_deref() == Some(trace_id);
        let full = trace_id.to_owned();
        h_flex()
            .min_w_0()
            .gap(r(8.))
            .child(
                div()
                    .id(SharedString::from(format!("trace-{key}")))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(r(11.))
                    .child(model::shorten_trace_id(trace_id))
                    .tooltip(tooltip(trace_id.to_owned().into())),
            )
            .child(
                Button::new(SharedString::from(format!("copy-trace-{key}")), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                    .icon(Icon::Copy)
                    .tooltip(if copied { "Copied" } else { "Copy full trace ID" })
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_trace_id(full.clone(), cx))),
            )
            .into_any_element()
    }

    /// One bounded trace table: a header row and its rows, or the empty line.
    fn trace_table(&self, title: &'static str, headers: &[(&'static str, f32, bool)], min_width: f32, rows: Vec<Vec<AnyElement>>, empty: &'static str, cx: &App) -> Block {
        let p = palette(cx);
        let divider = hsla(p.border).opacity(0.6);
        let content = if rows.is_empty() {
            div().px(r(20.)).py(r(16.)).text_xs().text_color(hsla(p.muted_foreground)).child(empty).into_any_element()
        } else {
            let header = h_flex().border_b_1().border_color(divider).children(headers.iter().enumerate().map(|(index, (label, width, end))| {
                Self::header_cell(label, *width, *end, cx).py(r(10.)).when(index == 0, |this| this.pl(r(20.))).when(index + 1 == headers.len(), |this| this.pr(r(20.)))
            }));
            let widths: Vec<f32> = headers.iter().map(|(_, width, _)| *width).collect();
            let count = widths.len();
            div()
                .id(SharedString::from(format!("trace-table-{title}")))
                .overflow_x_scroll()
                .child(v_flex().min_w(r(min_width)).text_xs().child(header).children(rows.into_iter().enumerate().map(|(row_index, cells)| {
                    h_flex().items_start().py(r(12.)).when(row_index > 0, |this| this.border_t_1().border_color(divider)).children(cells.into_iter().enumerate().map(
                        |(index, cell)| {
                            div()
                                .w(relative(widths[index]))
                                .min_w_0()
                                .px(r(12.))
                                .when(index == 0, |this| this.pl(r(20.)))
                                .when(index + 1 == count, |this| this.pr(r(20.)))
                                .child(cell)
                        },
                    ))
                })))
                .into_any_element()
        };
        Block::new(&[], self.diagnostics_section(title, None, vec![content], cx))
    }

    fn trace_tables(&mut self, now: i64, cx: &mut Context<Self>) -> Vec<Block> {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let initial = self.diagnostics.trace.initial_loading();
        let data = self.diagnostics.trace.data.clone();
        let name = |text: &str| div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(text.to_owned()).into_any_element();
        let number = |text: String| div().font_family(mono.clone()).child(text).into_any_element();
        let muted_number = |text: String| div().font_family(mono.clone()).text_color(hsla(p.muted_foreground)).child(text).into_any_element();
        let muted = |element: AnyElement| div().min_w_0().text_color(hsla(p.muted_foreground)).child(element).into_any_element();

        let latest: Vec<Vec<AnyElement>> = data
            .iter()
            .flat_map(|data| data.latest_failures.iter().enumerate())
            .map(|(index, failure)| {
                vec![
                    name(&failure.name),
                    muted(self.expandable(format!("latest-{index}"), &failure.cause, 3, "Show full error", cx)),
                    number(model::format_duration(failure.duration_ms)),
                    muted_number(model::relative_no_wrap(&failure.ended_at, now)),
                ]
            })
            .collect();
        let common: Vec<Vec<AnyElement>> = data
            .iter()
            .flat_map(|data| data.common_failures.iter().enumerate())
            .map(|(index, failure)| {
                vec![
                    name(&failure.name),
                    number(format_count(failure.count)),
                    muted(self.expandable(format!("common-{index}"), &failure.cause, 3, "Show full error", cx)),
                    muted_number(model::relative_no_wrap(&failure.last_seen_at, now)),
                ]
            })
            .collect();
        let slowest: Vec<Vec<AnyElement>> = data
            .iter()
            .flat_map(|data| data.slowest_spans.iter().enumerate())
            .map(|(index, span)| {
                vec![
                    name(&span.name),
                    number(model::format_duration(span.duration_ms)),
                    muted_number(model::relative_no_wrap(&span.ended_at, now)),
                    muted(self.trace_id_cell(&format!("slow-{index}"), &span.trace_id, cx)),
                ]
            })
            .collect();
        let logs: Vec<Vec<AnyElement>> = data
            .iter()
            .flat_map(|data| data.latest_warning_and_error_logs.iter().enumerate())
            .map(|(index, event)| {
                vec![
                    muted_number(model::relative_no_wrap(&event.seen_at, now)),
                    div()
                        .flex()
                        .child(
                            div()
                                .px(r(6.))
                                .py(r(2.))
                                .rounded(r(4.))
                                .bg(hsla(p.muted))
                                .font_family(mono.clone())
                                .text_size(r(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground).opacity(0.8))
                                .child(event.level.to_uppercase()),
                        )
                        .into_any_element(),
                    div().truncate().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(event.span_name.clone()).into_any_element(),
                    muted(self.expandable(format!("log-{index}"), &event.message, 2, "Show full message", cx)),
                    muted(self.trace_id_cell(&format!("log-{index}"), &event.trace_id, cx)),
                ]
            })
            .collect();
        let top: Vec<Vec<AnyElement>> = data
            .iter()
            .flat_map(|data| data.top_spans_by_count.iter())
            .map(|span| {
                vec![
                    name(&span.name),
                    number(format_count(span.count)),
                    number(format_count(span.failure_count)),
                    number(model::format_duration(span.average_duration_ms)),
                    number(model::format_duration(span.max_duration_ms)),
                ]
            })
            .collect();
        let empty = |loading: &'static str, done: &'static str| if initial { loading } else { done };
        vec![
            self.trace_table(
                "Latest Failures",
                &[("Span", 0.3, false), ("Cause", 0.4, false), ("Duration", 0.15, false), ("Ended", 0.15, false)],
                640.,
                latest,
                empty("Loading failures...", "No failed spans found."),
                cx,
            ),
            self.trace_table(
                "Most Common Failures",
                &[("Span", 0.3, false), ("Count", 0.1, false), ("Cause", 0.45, false), ("Last Seen", 0.15, false)],
                760.,
                common,
                empty("Loading failure groups...", "No repeated failures found."),
                cx,
            ),
            self.trace_table(
                "Slowest Spans",
                &[("Span", 0.44, false), ("Duration", 0.14, false), ("Ended", 0.12, false), ("Trace", 0.30, false)],
                900.,
                slowest,
                empty("Loading slow spans...", "No spans found."),
                cx,
            ),
            self.trace_table(
                "Span Logs",
                &[("Time", 0.11, false), ("Level", 0.09, false), ("Span", 0.24, false), ("Message", 0.26, false), ("Trace", 0.30, false)],
                920.,
                logs,
                empty("Loading recent logs...", "No warnings or errors found."),
                cx,
            ),
            self.trace_table(
                "Top Span Names",
                &[("Span", 0.48, false), ("Count", 0.13, false), ("Failures", 0.13, false), ("Average", 0.13, false), ("Max", 0.13, false)],
                760.,
                top,
                empty("Loading span names...", "No spans found."),
                cx,
            ),
        ]
    }

    fn telemetry_edited(&mut self, signal: OtlpSignal, cx: &mut Context<Self>) {
        let index = OtlpSignal::ALL.iter().position(|candidate| *candidate == signal).unwrap_or(0);
        let value = self.diagnostics.telemetry.inputs[index].read(cx).value().to_string();
        let saved = self.thread_settings(cx).map(|config| signal.saved(&config.settings.observability).to_owned()).unwrap_or_default();
        // A field back at its saved value follows the server again, so a save never overwrites
        // another client's change.
        if value.trim() == saved {
            self.diagnostics.telemetry.drafts.remove(&signal);
        } else {
            self.diagnostics.telemetry.drafts.insert(signal, value);
        }
        self.diagnostics.telemetry.message = None;
        self.diagnostics.telemetry.invalid.retain(|invalid| *invalid != signal);
        cx.notify();
    }

    fn telemetry_values(&self, cx: &App) -> Vec<(OtlpSignal, String)> {
        let saved = self.thread_settings(cx).map(|config| config.settings.observability.clone()).unwrap_or_default();
        OtlpSignal::ALL.iter().map(|signal| (*signal, self.diagnostics.telemetry.drafts.get(signal).cloned().unwrap_or_else(|| signal.saved(&saved).to_owned()))).collect()
    }

    fn save_telemetry(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.thread_settings(cx).map(|config| config.settings.observability.clone()) else {
            return;
        };
        let values = self.telemetry_values(cx);
        if self.diagnostics.telemetry.saving || model::changed_signals(&values, &saved).is_empty() || !self.may(SETTINGS_WRITE_SCOPE, cx) {
            return;
        }
        let payload = match model::telemetry_save_payload(&values, &saved) {
            Ok(payload) => payload,
            Err(invalid) => {
                self.diagnostics.telemetry.invalid = invalid;
                return cx.notify();
            }
        };
        self.diagnostics.telemetry.saving = true;
        self.diagnostics.telemetry.message = None;
        self.diagnostics.telemetry.invalid.clear();
        let view = cx.entity().downgrade();
        store::update_server_then(
            &self.state,
            ServerSettingChange::Prepared(payload),
            move |error, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.diagnostics.telemetry.saving = false;
                    if error.is_none() {
                        this.diagnostics.telemetry.drafts.clear();
                        this.diagnostics.telemetry.message = Some(model::TELEMETRY_SAVED);
                    }
                    cx.notify();
                });
            },
            cx,
        );
        cx.notify();
    }

    fn discard_telemetry(&mut self, can_check: bool, cx: &mut Context<Self>) {
        self.diagnostics.telemetry.invalid.clear();
        self.diagnostics.telemetry.drafts.clear();
        self.diagnostics.telemetry.message = None;
        if can_check {
            let config = self.thread_settings(cx).cloned();
            let running = config.as_ref().and_then(|config| config.observability.clone());
            let saved = config.map(|config| config.settings.observability).unwrap_or_default();
            for signal in OtlpSignal::ALL {
                if model::running_override(running.as_ref(), signal).is_some() {
                    continue;
                }
                let url = signal.saved(&saved).to_owned();
                if self.diagnostics.telemetry.checks.get(&signal).map(|check| check.url.as_str()) != Some(url.as_str()) {
                    self.check_endpoint(signal, url, cx);
                }
            }
        }
        cx.notify();
    }

    fn check_endpoint(&mut self, signal: OtlpSignal, url: String, cx: &mut Context<Self>) {
        if !model::is_http_url(&url) {
            return;
        }
        self.diagnostics.telemetry.checks.insert(signal, EndpointCheck { url: url.clone(), result: None });
        self.send_check(signal, url, cx);
    }

    fn send_check(&mut self, signal: OtlpSignal, url: String, cx: &mut Context<Self>) {
        let task = self.state.read(cx).rpc(model::CHECK_OTLP_ENDPOINT, signal.check_payload(&url));
        cx.spawn(async move |this, cx| {
            let result = task.await.ok().and_then(|reply| model::decode_check(&reply));
            let _ = this.update(cx, |this, cx| {
                let checks = &mut this.diagnostics.telemetry.checks;
                if checks.get(&signal).is_none_or(|check| check.url != url || check.result.is_some()) {
                    return;
                }
                match result {
                    Some(result) => {
                        checks.insert(signal, EndpointCheck { url, result: Some(result) });
                    }
                    None => {
                        checks.remove(&signal);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `TelemetryExportForm`.
    fn telemetry_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(config) = self.thread_settings(cx).cloned() else {
            return div().into_any_element();
        };
        let saved = config.settings.observability.clone();
        let running = config.observability.clone();
        let can_edit = self.may(SETTINGS_WRITE_SCOPE, cx);
        let can_check = can_edit && config.environment.capabilities.otlp_endpoint_check == Some(true);
        if !self.diagnostics.telemetry.seeded {
            self.diagnostics.telemetry.seeded = true;
            for signal in OtlpSignal::ALL {
                let url = model::running_override(running.as_ref(), signal).map_or_else(|| signal.saved(&saved).to_owned(), |found| found.url);
                if model::is_http_url(&url) {
                    self.diagnostics.telemetry.checks.insert(signal, EndpointCheck { url, result: None });
                }
            }
        }
        if can_check && !self.diagnostics.telemetry.sent {
            self.diagnostics.telemetry.sent = true;
            let pending: Vec<(OtlpSignal, String)> =
                self.diagnostics.telemetry.checks.iter().filter(|(_, check)| check.result.is_none()).map(|(signal, check)| (*signal, check.url.clone())).collect();
            for (signal, url) in pending {
                self.send_check(signal, url, cx);
            }
        }
        let values = self.telemetry_values(cx);
        for (index, (signal, value)) in values.iter().enumerate() {
            if !self.diagnostics.telemetry.drafts.contains_key(signal) {
                Self::sync_input(&self.diagnostics.telemetry.inputs[index], value, window, cx);
            }
        }
        let saving = self.diagnostics.telemetry.saving;
        let changed = !model::changed_signals(&values, &saved).is_empty();
        let environment = self.environment_label(cx);
        let dot_color = |dot: Dot| match dot {
            Dot::Muted => hsla(p.muted_foreground).opacity(0.4),
            Dot::Warning => hsla(p.warning),
            Dot::Success => hsla(p.success),
            Dot::Destructive => hsla(p.destructive),
        };
        let mut form = v_flex()
            .gap(r(16.))
            .px(r(20.))
            .py(r(16.))
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(model::TELEMETRY_DESCRIPTION))
            .when(!can_edit, |this| this.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(model::telemetry_no_permission(&environment))));
        for (index, (signal, value)) in values.iter().enumerate() {
            let signal = *signal;
            let found = model::running_override(running.as_ref(), signal);
            let url = found.as_ref().map_or_else(|| value.trim().to_owned(), |found| found.url.clone());
            let check = self.diagnostics.telemetry.checks.get(&signal);
            let status = model::signal_status(found.as_ref(), can_check.then(|| model::endpoint_status(&url, check)));
            let checking = check.is_some_and(|check| check.result.is_none());
            let running_line = model::running_line(signal, &saved, running.as_ref());
            let refused = self.diagnostics.telemetry.invalid.contains(&signal);
            let test_url = url.clone();
            form = form.child(
                v_flex()
                    .gap(r(6.))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(r(12.))
                            .child(
                                h_flex()
                                    .gap(r(6.))
                                    .children(status.as_ref().map(|(_, dot)| div().size(r(8.)).rounded_full().bg(dot_color(*dot))))
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(format!("{} endpoint", signal.label()))),
                            )
                            .children(status.map(|(label, _)| div().text_xs().text_color(hsla(p.muted_foreground)).child(label))),
                    )
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(div().flex_1().min_w_0().child(Input::new(&self.diagnostics.telemetry.inputs[index]).small().disabled(saving || !can_edit)))
                            .when(can_check, |this| {
                                this.child(
                                    Button::new(SharedString::from(format!("telemetry-test-{}", signal.as_str())), ButtonVariant::Outline, ButtonSize::Sm)
                                        .label("Test")
                                        .disabled(!model::is_http_url(&url) || checking)
                                        .on_click(cx.listener(move |this, _, _, cx| this.check_endpoint(signal, test_url.clone(), cx))),
                                )
                            }),
                    )
                    .when(refused, |this| this.child(div().text_xs().text_color(hsla(p.destructive_foreground)).child(model::ENDPOINT_HINT)))
                    .children(running_line.map(|(running, variable)| {
                        h_flex()
                            .flex_wrap()
                            .gap(r(4.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(format!("Running: {running}"))
                            .children(variable.map(|variable| h_flex().gap(r(4.)).child("from").child(div().font_family(cx.theme().mono_font_family.clone()).child(variable))))
                    })),
            );
        }
        let message = self.diagnostics.telemetry.message;
        form = form.child(
            h_flex().justify_between().gap(r(12.)).child(div().text_xs().text_color(hsla(p.muted_foreground)).children(message)).child(
                h_flex()
                    .flex_none()
                    .gap(r(8.))
                    .child(
                        Button::new("telemetry-discard", ButtonVariant::Outline, ButtonSize::Xs)
                            .label("Discard")
                            .disabled(!changed || saving || !can_edit)
                            .on_click(cx.listener(move |this, _, _, cx| this.discard_telemetry(can_check, cx))),
                    )
                    .child(
                        Button::new("telemetry-save", ButtonVariant::Default, ButtonSize::Xs)
                            .label(if saving { "Saving\u{2026}" } else { "Save" })
                            .disabled(!changed || saving || !can_edit)
                            .on_click(cx.listener(|this, _, _, cx| this.save_telemetry(cx))),
                    ),
            ),
        );
        let highlighted = self.is_target("telemetry-export");
        div()
            .id("telemetry-export")
            .rounded(r(12.))
            .when(highlighted, |this| this.bg(hsla(p.accent).opacity(0.3)))
            .child(self.diagnostics_section("OpenTelemetry export", None, vec![form.into_any_element()], cx))
            .into_any_element()
    }
}
