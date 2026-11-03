//! `ResourceTelemetryDiagnostics.tsx`: static history bars and virtualized process tables.
use super::*;
use std::sync::Arc;
use t3_client::resource_telemetry::{self as client, Feed};
use t3_core::resource_telemetry::{self as monitor, BooleanState, Category, History, Identity, Snapshot, SourceStatus, ThermalState};

#[derive(Default)]
pub(super) struct MonitorState {
    feed: Feed,
    history: monitor::HistoryState,
    history_task: Option<Task<()>>,
    retry: Query<()>,
    stream: Option<Task<()>>,
    connection: Option<u64>,
    collapsed: HashSet<Identity>,
    fixture: bool,
}
impl MonitorState {
    pub(super) fn leave(&mut self) {
        self.stream = None;
        self.fixture = false;
        self.connection = None;
        self.feed = Feed::default();
        self.history_task = None;
        self.history.reset();
        self.retry.reset();
        self.collapsed.clear();
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct Fixture {
    pub snapshot: Option<Snapshot>,
    pub history: Option<History>,
    pub error: Option<String>,
    #[serde(default)]
    pub history_error: Option<String>,
    #[serde(default)]
    pub history_pending: bool,
}
impl gpui::Global for Fixture {}

impl SettingsView {
    pub(super) fn watch_resource_monitor(&mut self, cx: &mut Context<Self>) {
        if let Some(fixture) = cx.try_global::<Fixture>() {
            if !self.diagnostics.monitor.fixture {
                self.diagnostics.monitor.feed = Feed { snapshot: fixture.snapshot.clone().map(Arc::new), error: fixture.error.clone() };
                self.diagnostics.monitor.history.query.data = fixture.history.clone().map(Arc::new);
                self.diagnostics.monitor.fixture = true;
            }
            return;
        }
        let connection = self.state.read(cx).connection_id();
        if self.diagnostics.monitor.connection == connection {
            return;
        }
        self.diagnostics.monitor.leave();
        self.start_resource_subscription(cx);
        self.load_monitor_history(cx);
    }

    fn start_resource_subscription(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connection = state.connection_id();
        let Some(handle) = state.client_handle() else {
            return;
        };
        let mut stream = client::subscribe(state.runtime(), handle);
        self.diagnostics.monitor.connection = connection;
        self.diagnostics.monitor.stream = Some(cx.spawn(async move |this, cx| {
            while stream.feed.changed().await.is_ok() {
                let feed = stream.feed.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        this.diagnostics.monitor.feed = feed;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn load_monitor_history(&mut self, cx: &mut Context<Self>) {
        self.diagnostics.monitor.history_task = None;
        let request = self.diagnostics.monitor.history.query.begin();
        if let Some(fixture) = cx.try_global::<Fixture>() {
            if !fixture.history_pending
                && let Some(error) = &fixture.history_error
            {
                self.diagnostics.monitor.history.query.finish(request, Err(error.clone()));
            }
            return;
        }
        let state = self.state.read(cx);
        let Some(handle) = state.client_handle() else {
            self.diagnostics.monitor.history.query.finish(request, Err("The connection closed. Try again once it reconnects.".into()));
            return;
        };
        let window = monitor::HISTORY_WINDOWS[self.diagnostics.monitor.history.selected];
        let mut request_task = client::start_history(state.runtime(), handle, window);
        self.diagnostics.monitor.history_task = Some(cx.spawn(async move |this, cx| {
            let result = request_task.result().await;
            let _ = this.update(cx, |this, cx| {
                if this.diagnostics.monitor.history.query.finish(request, result) {
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn refresh_monitor(&mut self, cx: &mut Context<Self>) {
        if self.diagnostics.monitor.fixture {
            return;
        }
        self.diagnostics.monitor.stream = None;
        self.start_resource_subscription(cx);
        cx.notify();
    }

    fn retry_monitor(&mut self, cx: &mut Context<Self>) {
        if !self.may(model::ENVIRONMENT_MAINTAIN_SCOPE, cx) || !self.may("diagnostics:read", cx) || self.diagnostics.monitor.retry.pending || self.diagnostics.monitor.fixture {
            return;
        }
        let state = self.state.read(cx);
        let Some(handle) = state.client_handle() else {
            return;
        };
        let task = state.runtime().spawn(async move { client::retry(&handle).await });
        let request = self.diagnostics.monitor.retry.begin();
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update(cx, |this, cx| {
                let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
                if !this.diagnostics.monitor.retry.finish(request, outcome) {
                    return;
                }
                match result {
                    Ok(_) => {
                        this.refresh_monitor(cx);
                        this.load_monitor_history(cx);
                    }
                    Err(error) => this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not restart resource monitor").description(error), cx)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn resource_monitor_blocks(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<Block> {
        vec![
            Block::new(&[], self.monitor_footprint(cx)),
            Block::new(&[], self.monitor_host(cx)),
            Block::new(&[], self.monitor_timeline(window, cx)),
            Block::new(&[], self.monitor_processes(window, cx)),
            Block::new(&[], self.monitor_attribution(window, cx)),
        ]
    }

    fn monitor_note(text: impl Into<SharedString>, cx: &App) -> AnyElement {
        div().px(r(20.)).py(r(16.)).text_xs().text_color(hsla(palette(cx).muted_foreground)).child(text.into()).into_any_element()
    }

    fn monitor_status(label: &str, status: SourceStatus, desktop_only: bool, cx: &App) -> AnyElement {
        let p = palette(cx);
        let color = if desktop_only {
            hsla(p.muted_foreground)
        } else {
            match status {
                SourceStatus::Healthy => hsla(p.success_foreground),
                SourceStatus::Starting | SourceStatus::Degraded => hsla(p.warning_foreground),
                _ => hsla(p.destructive),
            }
        };
        div()
            .px(r(10.))
            .py(r(4.))
            .rounded_full()
            .border_1()
            .border_color(color.opacity(0.3))
            .bg(color.opacity(0.1))
            .text_size(r(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color)
            .child(format!("{label} {}", if desktop_only { "Desktop only" } else { status.label() }))
            .into_any_element()
    }

    fn monitor_footprint(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let feed = &self.diagnostics.monitor.feed;
        let snapshot = feed.snapshot.as_deref();
        let all = snapshot.map(|s| &s.groups.all_t3);
        let dots = |value: Option<String>| value.unwrap_or_else(|| "...".into());
        let header = v_flex()
            .px(r(20.))
            .py(r(16.))
            .gap(r(4.))
            .bg(hsla(p.muted).opacity(0.2))
            .child(div().text_size(r(10.)).font_weight(FontWeight::SEMIBOLD).child("T3 SYSTEM FOOTPRINT"))
            .child(
                div().text_xs().text_color(hsla(p.muted_foreground)).child("Live native counters for the server, providers, terminals, desktop processes, and the monitor itself."),
            )
            .child(
                div()
                    .text_size(r(10.))
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("Sampling every {}", snapshot.map(|s| monitor::sample_interval(s.sample_interval_ms)).unwrap_or_else(|| "...".into()))),
            );
        let stats = [
            ("Current CPU", dots(all.map(|a| format!("{:.1}%", a.current_cpu_percent))), all.map(|a| format!("{} observed CPU time", monitor::cpu_time(a.cpu_time_ms)))),
            ("Resident memory", dots(all.map(|a| monitor::bytes(a.current_rss_bytes))), all.map(|a| format!("{} combined process peaks", monitor::bytes(a.peak_rss_bytes)))),
            ("Process count", dots(all.map(|a| a.process_count.to_string())), all.map(|a| format!("{} starts · {} exits", a.process_starts, a.process_exits))),
            ("Read throughput", dots(all.map(|a| monitor::rate(a.io_read_bytes_per_second))), all.map(|a| format!("{} observed", monitor::bytes(a.io_read_bytes)))),
            ("Write throughput", dots(all.map(|a| monitor::rate(a.io_write_bytes_per_second))), all.map(|a| format!("{} observed", monitor::bytes(a.io_write_bytes)))),
            (
                "CPU speed limit",
                dots(snapshot.map(|s| s.speed_limit_percent.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "Unknown".into()))),
                snapshot.map(|s| format!("{} thermal state", s.power.thermal_state.label())),
            ),
        ];
        let mut content = vec![header.into_any_element()];
        let mut stats = stats.into_iter();
        for _ in 0..2 {
            content.push(
                h_flex()
                    .children(stats.by_ref().take(3).map(|(label, value, detail)| {
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .px(r(20.))
                            .py(r(16.))
                            .border_t_1()
                            .border_color(hsla(p.border).opacity(0.55))
                            .child(div().text_size(r(10.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child(label.to_uppercase()))
                            .child(
                                div()
                                    .mt(r(10.))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(r(24.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if label == "Write throughput" && all.is_some_and(|a| a.io_write_bytes_per_second >= 10. * 1024. * 1024.) {
                                        hsla(p.destructive)
                                    } else if (label == "Write throughput" && all.is_some_and(|a| a.io_write_bytes_per_second >= 1024. * 1024.))
                                        || (label == "CPU speed limit" && snapshot.and_then(|s| s.speed_limit_percent).is_some_and(|v| v < 80.))
                                    {
                                        hsla(p.warning_foreground)
                                    } else {
                                        hsla(p.foreground)
                                    })
                                    .child(value),
                            )
                            .children(detail.map(|s| div().mt(r(6.)).text_size(r(10.)).text_color(hsla(p.muted_foreground).opacity(0.6)).child(s)))
                    }))
                    .into_any_element(),
            );
        }
        content.extend(Self::error_lines(feed.error.clone().into_iter().map(|e| (e, hsla(p.destructive))).collect(), cx));
        if let Some(s) = snapshot {
            content.push(
                h_flex()
                    .children(
                        [("Backend + agents", &s.groups.backend, p.success), ("Desktop", &s.groups.electron, p.info), ("Monitor overhead", &s.groups.monitor, p.warning)].map(
                            |(label, a, color)| {
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .px(r(20.))
                                    .py(r(16.))
                                    .border_t_2()
                                    .border_color(hsla(color).opacity(0.8))
                                    .child(div().text_size(r(10.)).text_color(hsla(p.muted_foreground)).child(label.to_uppercase()))
                                    .child(div().text_size(r(10.)).child(format!("{} {}", a.process_count, if a.process_count == 1 { "process" } else { "processes" })))
                                    .children(
                                        [
                                            [("CPU", format!("{:.1}%", a.current_cpu_percent)), ("Memory", monitor::bytes(a.current_rss_bytes))],
                                            [("Read", monitor::rate(a.io_read_bytes_per_second)), ("Write", monitor::rate(a.io_write_bytes_per_second))],
                                        ]
                                        .map(|pairs| {
                                            h_flex().mt(r(10.)).gap(r(16.)).children(pairs.map(|(label, value)| {
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap(r(2.))
                                                    .child(div().text_size(r(10.)).text_color(hsla(p.muted_foreground).opacity(0.45)).child(label.to_uppercase()))
                                                    .child(div().text_xs().font_family(cx.theme().mono_font_family.clone()).child(value))
                                            }))
                                        }),
                                    )
                            },
                        ),
                    )
                    .into_any_element(),
            );
        }
        let actions = h_flex()
            .gap(r(8.))
            .children(snapshot.map(|s| Self::monitor_status("Native", s.health.native.status, false, cx)))
            .child(
                div()
                    .text_size(r(11.))
                    .text_color(hsla(p.muted_foreground))
                    .child(snapshot.map(|s| format!("Updated {}", model::relative_no_wrap(&s.read_at, now_ms()))).unwrap_or_else(|| "Waiting for sample".into())),
            )
            .child(Self::refresh_button(
                "refresh-resource-telemetry",
                "Refresh telemetry snapshot",
                snapshot.is_none() && feed.error.is_none(),
                cx.listener(|this, _, _, cx| this.refresh_monitor(cx)),
            ))
            .into_any_element();
        self.diagnostics_section("Resource monitor", Some(actions), vec![v_flex().children(content).into_any_element()], cx)
    }

    fn monitor_detail(label: &'static str, value: impl Into<SharedString>, cx: &App) -> AnyElement {
        let p = palette(cx);
        h_flex()
            .justify_between()
            .gap(r(16.))
            .py(r(10.))
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.5))
            .text_size(r(11.))
            .child(div().text_color(hsla(p.muted_foreground).opacity(0.75)).child(label))
            .child(div().min_w_0().truncate().font_family(cx.theme().mono_font_family.clone()).child(value.into()))
            .into_any_element()
    }

    fn monitor_host(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let snapshot = self.diagnostics.monitor.feed.snapshot.as_deref();
        let mut host = vec![div().mb(r(12.)).text_size(r(10.)).font_weight(FontWeight::SEMIBOLD).child("HOST STATE").into_any_element()];
        let has_power = snapshot.is_some_and(|s| {
            [s.power.on_battery, s.power.low_power_mode, s.power.idle, s.power.locked].iter().any(|v| *v != BooleanState::Unknown) || s.power.thermal_state != ThermalState::Unknown
        });
        if let Some(s) = snapshot.filter(|_| has_power) {
            host.extend([
                Self::monitor_detail("Power source", monitor::boolean_label(s.power.on_battery, "Battery", "External power"), cx),
                Self::monitor_detail("Low power mode", monitor::boolean_label(s.power.low_power_mode, "Enabled", "Disabled"), cx),
                Self::monitor_detail(
                    "Idle",
                    format!("{}{}", monitor::boolean_label(s.power.idle, "Idle", "Active"), s.power.idle_seconds.map(|v| format!(" · {v:.0}s")).unwrap_or_default()),
                    cx,
                ),
                Self::monitor_detail("Session", if s.power.suspended { "Suspended" } else { monitor::boolean_label(s.power.locked, "Locked", "Unlocked") }, cx),
                Self::monitor_detail("Thermal", s.power.thermal_state.label(), cx),
            ]);
        } else {
            host.push(
                v_flex()
                    .gap(r(6.))
                    .p(r(16.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Desktop host signals not connected"))
                    .child(
                        div()
                            .text_size(r(11.))
                            .text_color(hsla(p.muted_foreground))
                            .child("Power, idle, lock, and thermal state are supplied by the desktop host. Process telemetry remains fully active in this browser session."),
                    )
                    .into_any_element(),
            );
        }
        let mut health = vec![div().mb(r(12.)).text_size(r(10.)).font_weight(FontWeight::SEMIBOLD).child("COLLECTION HEALTH").into_any_element()];
        if let Some(s) = snapshot {
            for (label, source) in [("Native process monitor", &s.health.native), ("Electron main process", &s.health.desktop)] {
                let desktop_only = source.status == SourceStatus::Unavailable && source.last_error.as_ref().is_some_and(|s| s.contains("'web' mode"));
                health.push(
                    v_flex()
                        .py(r(12.))
                        .gap(r(4.))
                        .border_t_1()
                        .border_color(hsla(p.border).opacity(0.5))
                        .child(h_flex().justify_between().gap(r(8.)).child(div().text_sm().child(label)).child(Self::monitor_status("", source.status, desktop_only, cx)))
                        .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child(if desktop_only {
                            "Available when this page runs inside the desktop app.".into()
                        } else {
                            source.last_error.clone().unwrap_or_else(|| "No reported errors".into())
                        }))
                        .into_any_element(),
                );
            }
            health.extend([
                Self::monitor_detail("Collection time", monitor::duration_micros(s.health.collection_duration_micros), cx),
                Self::monitor_detail("Process scan", format!("{}/{} retained", s.health.retained_process_count, s.health.scanned_process_count), cx),
                Self::monitor_detail("Inaccessible", s.health.inaccessible_process_count.to_string(), cx),
                Self::monitor_detail(
                    "Sidecar",
                    s.health
                        .sidecar_version
                        .as_ref()
                        .map(|v| format!("{v}{}", s.health.sidecar_pid.map(|pid| format!(" · PID {pid}")).unwrap_or_default()))
                        .unwrap_or_else(|| "Unavailable".into()),
                    cx,
                ),
                Self::monitor_detail("Restarts", s.health.restart_count.to_string(), cx),
            ]);
        } else {
            health.push(Self::monitor_note("Waiting for collector health.", cx));
        }
        let actions = monitor::should_show_retry(snapshot.map(|s| s.health.native.status), self.diagnostics.monitor.feed.error.as_deref()).then(|| {
            Button::new("retry-resource-monitor", ButtonVariant::Outline, ButtonSize::Xs)
                .label("Retry monitor")
                .disabled(self.diagnostics.monitor.retry.pending || !self.may(model::ENVIRONMENT_MAINTAIN_SCOPE, cx))
                .on_click(cx.listener(|this, _, _, cx| this.retry_monitor(cx)))
                .into_any_element()
        });
        let content = h_flex()
            .items_start()
            .child(v_flex().flex_1().min_w_0().px(r(20.)).py(r(16.)).children(host))
            .child(v_flex().flex_1().min_w_0().border_l_1().border_color(hsla(p.border).opacity(0.6)).px(r(20.)).py(r(16.)).children(health))
            .into_any_element();
        self.diagnostics_section("Host & collection", actions, vec![content], cx)
    }

    fn monitor_timeline(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let query = &self.diagnostics.monitor.history.query;
        let data = query.data.clone();
        let buckets = data.as_ref().map(|h| h.buckets.as_slice()).unwrap_or_default();
        let max_cpu = monitor::history_cpu_scale_max(buckets);
        let max_io = buckets.iter().fold(1_f64, |max, b| max.max(b.io_read_bytes + b.io_write_bytes));
        let chart = v_flex()
            .gap(r(12.))
            .px(r(20.))
            .py(r(16.))
            .child(
                h_flex().gap(r(16.)).text_size(r(10.)).children(
                    [("CPU average", p.foreground), ("I/O reads", p.info), ("I/O writes", p.warning)]
                        .map(|(label, color)| h_flex().gap(r(6.)).child(div().w(r(12.)).h(r(6.)).rounded_full().bg(hsla(color).opacity(0.7))).child(label)),
                ),
            )
            .child(
                h_flex()
                    .h(r(128.))
                    .items_end()
                    .gap(r(4.))
                    .px(r(8.))
                    .pt(r(12.))
                    .pb(r(8.))
                    .rounded(r(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.4))
                    .overflow_hidden()
                    .children(buckets.iter().enumerate().map(|(index, b)| {
                        h_flex()
                            .id(("resource-bucket", index))
                            .flex_1()
                            .min_w(r(4.))
                            .h_full()
                            .items_end()
                            .gap(gpui::px(1.))
                            .children([(b.avg_cpu_percent, max_cpu, 2., p.foreground), (b.io_read_bytes, max_io, 1., p.info), (b.io_write_bytes, max_io, 1., p.warning)].map(
                                |(value, max, min, color)| {
                                    div().flex_1().h(relative(monitor::history_bar_height(value, max, min) as f32 / 100.)).rounded_t(r(2.)).bg(hsla(color).opacity(0.7))
                                },
                            ))
                            .tooltip(tooltip(
                                format!(
                                    "{}\nCPU avg {:.1}%\nCPU peak {:.1}%\nRead {}\nWrite {}",
                                    b.started_at,
                                    b.avg_cpu_percent,
                                    b.max_cpu_percent,
                                    monitor::bytes(b.io_read_bytes),
                                    monitor::bytes(b.io_write_bytes)
                                )
                                .into(),
                            ))
                    })),
            )
            .into_any_element();
        let selected = self.diagnostics.monitor.history.selected;
        let windows = h_flex().gap(r(2.)).children(monitor::HISTORY_WINDOWS.iter().enumerate().map(|(index, option)| {
            Button::new(("resource-window", index), if index == selected { ButtonVariant::Outline } else { ButtonVariant::GhostMuted }, ButtonSize::Xs)
                .label(option.label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.diagnostics.monitor.history.selected != index {
                        this.diagnostics.monitor.history.select(index);
                        this.load_monitor_history(cx);
                        cx.notify();
                    }
                }))
        }));
        let actions = h_flex()
            .gap(r(8.))
            .child(windows)
            .child(Self::refresh_button("refresh-resource-timeline", "Refresh resource history", query.pending, cx.listener(|this, _, _, cx| this.load_monitor_history(cx))))
            .into_any_element();
        let mut content: Vec<_> = Self::error_lines(query.error.clone().into_iter().map(|e| (e, hsla(p.destructive))).collect(), cx).into_iter().collect();
        content.push(chart);
        content.push(self.monitor_table(MonitorTable::History, window, cx));
        self.diagnostics_section("Resource timeline", Some(actions), vec![v_flex().children(content).into_any_element()], cx)
    }

    fn monitor_processes(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let actions = div().text_size(r(10.)).text_color(hsla(palette(cx).muted_foreground).opacity(0.55)).child("Identity: PID + start time").into_any_element();
        let table = self.monitor_table(MonitorTable::Live, window, cx);
        self.diagnostics_section("Live process tree", Some(actions), vec![table], cx)
    }

    fn monitor_attribution(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let content = v_flex().child(Self::monitor_note("Native counters identify which process is reading or writing. These application-level counters identify known T3 operations so process spikes can be correlated with specific persistence and logging paths.",cx))
            .child(self.monitor_table(MonitorTable::Attribution,window,cx)).into_any_element();
        let actions = div().text_size(r(10.)).text_color(hsla(palette(cx).muted_foreground).opacity(0.55)).child("Logical bytes by operation").into_any_element();
        self.diagnostics_section("Instrumented application I/O", Some(actions), vec![content], cx)
    }

    fn monitor_table(&self, kind: MonitorTable, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let state = &self.diagnostics.monitor;
        let visible = state.feed.snapshot.as_ref().map(|s| monitor::visible_processes(&s.processes, &state.collapsed)).unwrap_or_default();
        let count = match kind {
            MonitorTable::Live => visible.len(),
            MonitorTable::History => state.history.query.data.as_ref().map_or(0, |h| h.top_processes.len()),
            MonitorTable::Attribution => state.feed.snapshot.as_ref().map_or(0, |s| s.attribution.entries.len()),
        };
        let header = h_flex()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.card))
            .children(kind.columns().iter().enumerate().map(|(i, (label, width))| Self::header_cell(label, *width, i >= 2, cx)));
        let body = if count == 0 {
            Self::monitor_note(
                match kind {
                    MonitorTable::Live => "Waiting for the native process monitor.",
                    MonitorTable::History => "No retained process samples in this window.",
                    MonitorTable::Attribution => "No instrumented application I/O has been recorded yet.",
                },
                cx,
            )
        } else {
            uniform_list(
                kind.id(),
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|index| {
                            let index = if matches!(kind, MonitorTable::Live) { *visible.get(index)? } else { index };
                            this.monitor_row(kind, index, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .h(r(table_height(count, window).min(if matches!(kind, MonitorTable::History) { 448. } else { 768. })))
            .into_any_element()
        };
        div()
            .id(kind.id())
            .overflow_x_scroll()
            .border_t_1()
            .border_color(hsla(p.border).opacity(0.6))
            .child(v_flex().min_w(r(kind.min_width())).text_xs().child(header).child(body))
            .into_any_element()
    }

    fn monitor_row(&self, kind: MonitorTable, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = palette(cx);
        let state = &self.diagnostics.monitor;
        let (values, command) = match kind {
            MonitorTable::Live => {
                let process = state.feed.snapshot.as_ref()?.processes.get(index)?;
                (
                    vec![
                        monitor::process_name(process),
                        monitor::category_label(process.category).into(),
                        format!("{:.1}%", process.cpu_percent),
                        monitor::cpu_time(process.cpu_time_ms),
                        monitor::bytes(process.resident_bytes),
                        monitor::rate(process.io_read_bytes_per_second),
                        monitor::rate(process.io_write_bytes_per_second),
                        monitor::bytes(process.io_read_bytes),
                        monitor::bytes(process.io_write_bytes),
                        process.identity.pid.to_string(),
                    ],
                    process.command.clone(),
                )
            }
            MonitorTable::History => {
                let process = state.history.query.data.as_ref()?.top_processes.get(index)?;
                (
                    vec![
                        if process.name.is_empty() { process.command.clone() } else { process.name.clone() },
                        monitor::category_label(process.category).into(),
                        monitor::cpu_time(process.cpu_time_ms),
                        format!("{:.1}%", process.max_cpu_percent),
                        monitor::bytes(process.peak_rss_bytes),
                        monitor::bytes(process.io_read_bytes),
                        monitor::bytes(process.io_write_bytes),
                        process.sample_count.to_string(),
                        process.identity.pid.to_string(),
                    ],
                    process.command.clone(),
                )
            }
            MonitorTable::Attribution => {
                let entry = state.feed.snapshot.as_ref()?.attribution.entries.get(index)?;
                (
                    vec![
                        entry.component.clone(),
                        entry.operation.clone(),
                        monitor::bytes(entry.logical_read_bytes),
                        monitor::bytes(entry.logical_write_bytes),
                        entry.count.to_string(),
                        monitor::attribution_time(entry.duration_ms),
                    ],
                    String::new(),
                )
            }
        };
        let mut row = h_flex().w_full().id((kind.id(), index)).h(r(PROCESS_ROW)).border_b_1().border_color(hsla(p.border).opacity(0.5)).hover(|s| s.bg(hsla(p.muted).opacity(0.2)));
        for (i, ((_, width), text)) in kind.columns().iter().zip(values).enumerate() {
            let mut cell = Self::cell(*width, i >= 2).flex_none().text_color(hsla(p.foreground));
            if i >= 2 {
                cell = cell.font_family(cx.theme().mono_font_family.clone());
            }
            if i == 0 {
                let name = div()
                    .id((SharedString::from(format!("{}-name", kind.id())), index))
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(text.clone())
                    .tooltip(tooltip(if command.is_empty() { text.into() } else { command.clone().into() }));
                if matches!(kind, MonitorTable::Live) {
                    let process = state.feed.snapshot.as_ref()?.processes.get(index)?;
                    let identity = process.identity.clone();
                    let collapsed = state.collapsed.contains(&identity);
                    let toggle = if !process.child_pids.is_empty() {
                        Button::new(("resource-collapse", index), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                            .icon(if collapsed { Icon::ChevronRight } else { Icon::ChevronDown })
                            .tooltip(format!("{} {}", if collapsed { "Expand" } else { "Collapse" }, monitor::process_name(process)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.diagnostics.monitor.collapsed.remove(&identity) {
                                    this.diagnostics.monitor.collapsed.insert(identity.clone());
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    } else {
                        div().size(r(20.)).flex_none().into_any_element()
                    };
                    let color = match process.category {
                        Category::ResourceMonitor => p.warning,
                        Category::ElectronMain | Category::ElectronRenderer | Category::ElectronGpu | Category::ElectronUtility => p.info,
                        Category::Server => p.status_input_icon,
                        _ => p.success,
                    };
                    cell = cell.child(
                        h_flex()
                            .min_w_0()
                            .gap(r(8.))
                            .pl(gpui::px(process.depth.min(7) as f32 * 10.))
                            .child(toggle)
                            .child(div().size(r(6.)).flex_none().rounded_full().bg(hsla(color)))
                            .child(name),
                    );
                } else {
                    cell = cell.child(name);
                }
            } else {
                let mut value = div().min_w_0().truncate().child(text);
                if matches!(kind, MonitorTable::Live) && i == 8 {
                    let process = state.feed.snapshot.as_ref()?.processes.get(index)?;
                    cell = cell.child(value.id(("resource-io-semantics", index)).tooltip(tooltip(monitor::io_label(process.io_semantics).into())));
                } else {
                    if (matches!(kind, MonitorTable::Live | MonitorTable::History) && i == 5) || (matches!(kind, MonitorTable::Attribution) && i == 2) {
                        value = value.text_color(hsla(p.info_foreground));
                    }
                    if (matches!(kind, MonitorTable::Live | MonitorTable::History) && i == 6) || (matches!(kind, MonitorTable::Attribution) && i == 3) {
                        value = value.text_color(hsla(p.warning_foreground));
                    }
                    cell = cell.child(value);
                }
            }
            row = row.child(cell);
        }
        Some(row.into_any_element())
    }
}

#[derive(Clone, Copy)]
enum MonitorTable {
    Live,
    History,
    Attribution,
}
impl MonitorTable {
    fn id(self) -> &'static str {
        match self {
            Self::Live => "resource-live",
            Self::History => "resource-history",
            Self::Attribution => "resource-attribution",
        }
    }
    fn min_width(self) -> f32 {
        match self {
            Self::Live => 1260.,
            Self::History => 1020.,
            Self::Attribution => 720.,
        }
    }
    fn columns(self) -> &'static [(&'static str, f32)] {
        match self {
            Self::Live => &[
                ("Process", 0.22),
                ("Category", 0.10),
                ("CPU", 0.07),
                ("CPU Time", 0.08),
                ("Memory", 0.09),
                ("Read/s", 0.09),
                ("Write/s", 0.09),
                ("Read Total", 0.10),
                ("Write Total", 0.09),
                ("PID", 0.07),
            ],
            Self::History => &[
                ("Process", 0.24),
                ("Category", 0.11),
                ("CPU Time", 0.10),
                ("Peak CPU", 0.10),
                ("Peak Mem", 0.11),
                ("Read", 0.11),
                ("Write", 0.11),
                ("Samples", 0.07),
                ("PID", 0.05),
            ],
            Self::Attribution => &[("Component", 0.22), ("Operation", 0.28), ("Logical Read", 0.14), ("Logical Write", 0.14), ("Count", 0.10), ("Time", 0.12)],
        }
    }
}
