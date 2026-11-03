//! `WelcomeWizard.tsx` ImportStep and `useProjectScans.ts`, for the connected environment.
use super::{Step, WelcomeWizard, key_stop, step_title};
use crate::provider_icon::{ProviderIcon, ProviderMark};
use crate::{
    icons::Icon,
    settings::controls::{Button, ButtonSize, ButtonVariant, r},
    sidebar::controller::uuid,
    theme::{hsla, palette},
    toast::{Toast, ToastKind},
};
use gpui::{AnyElement, Context, ElementId, IntoElement, ParentElement, Styled, Task, Window, div, prelude::*, uniform_list};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use serde_json::{Value, json};
use std::{collections::HashSet, rc::Rc, time::Duration};
use t3_core::{auth::ORCHESTRATION_OPERATE_SCOPE, onboarding::projects as model, projects::PROJECTS_MUTATE, sidebar::time::parse_iso};

pub(super) struct RequestFailure {
    message: String,
    pub(super) definitive: bool,
}
impl std::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl From<t3_client::session::RpcError> for RequestFailure {
    fn from(error: t3_client::session::RpcError) -> Self {
        let definitive = matches!(&error,t3_client::session::RpcError::Failure(failure) if failure.message != "Interrupted");
        Self { message: error.to_string(), definitive }
    }
}

#[derive(Clone)]
enum Row {
    Candidate { index: usize, label: Option<String>, nested: bool },
    Group { key: String, label: String, indices: Vec<usize>, threads: u64 },
}
#[derive(Default)]
pub(super) struct Projects {
    connection: Option<u64>,
    demand: model::Demand,
    environment: String,
    scan: Option<model::Scan>,
    error: Option<String>,
    selected: HashSet<String>,
    collapsed: HashSet<String>,
    rows: Rc<Vec<Row>>,
    progress: model::Progress,
    busy: bool,
    retry_after_scan: bool,
    pending: Option<Task<()>>,
    grants: Option<Task<()>>,
    landing: Option<String>,
    landing_timeout: Option<Task<()>>,
}
impl Projects {
    fn rebuild(&mut self) {
        let Some(scan) = &self.scan else { return };
        let (groups, other) = model::group(&scan.candidates);
        let mut rows = Vec::new();
        for g in groups {
            if g.candidates.len() == 1 {
                rows.push(Row::Candidate {
                    index: g.candidates[0],
                    label: Some(g.label),
                    nested: false,
                });
            } else {
                rows.push(Row::Group {
                    key: g.key.clone(),
                    label: g.label,
                    indices: g.candidates.clone(),
                    threads: g.thread_count,
                });
                if !self.collapsed.contains(&g.key) {
                    rows.extend(g.candidates.into_iter().map(|index| Row::Candidate { index, label: None, nested: true }));
                }
            }
        }
        if !other.is_empty() {
            rows.push(Row::Group {
                key: "other".into(),
                label: "Other folders".into(),
                indices: other.clone(),
                threads: 0,
            });
            if !self.collapsed.contains("other") {
                rows.extend(other.into_iter().map(|index| Row::Candidate { index, label: None, nested: true }));
            }
        }
        self.rows = Rc::new(rows);
    }
}

impl WelcomeWizard {
    fn projects_current(&self, cx: &Context<Self>) -> bool {
        self.open && self.step == Step::Projects && self.projects.connection.is_some() && self.projects.connection == self.state.read(cx).connection_id() && self.projects.environment == self.project_environment(cx).unwrap_or_default()
    }
    fn project_environment(&self, cx: &Context<Self>) -> Option<String> {
        let app = self.state.read(cx);
        let client = app.client_handle()?;
        Some(
            app.server_config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.as_str())
                .filter(|id| !id.is_empty())
                .unwrap_or(client.environment())
                .to_owned(),
        )
    }
    fn projects_access(&self, scope: &str, cx: &Context<Self>) -> bool {
        #[cfg(feature = "snapshot")]
        if scope == ORCHESTRATION_OPERATE_SCOPE && std::env::var_os("T3_NATIVE_ONBOARDING_READ_ONLY").is_some() {
            return false;
        }
        self.projects_current(cx) && self.state.read(cx).client_handle().is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|grants| grants.grants(scope)))
    }
    pub(super) fn cancel_projects(&mut self) {
        self.projects.demand.cancel();
        self.projects.pending = None;
        self.projects.grants = None;
        self.projects.landing_timeout = None;
        self.projects.landing = None;
        self.projects.busy = false;
        self.projects.retry_after_scan = false;
    }
    pub(super) fn projects_changed(&mut self, cx: &mut Context<Self>) {
        if self.projects.connection.is_none() {
            return;
        }
        if self.projects.connection != self.state.read(cx).connection_id() || (self.projects.connection.is_some() && self.projects.environment != self.project_environment(cx).unwrap_or_default()) {
            self.cancel_projects();
            self.projects = Projects::default();
            if self.open && self.step == Step::Projects {
                self.go_to(Step::Connection, cx);
            }
        }
        if self.projects_current(cx) && self.projects.landing.is_some() {
            self.try_import_landing(cx);
        }
    }
    pub(super) fn enter_projects(&mut self, cx: &mut Context<Self>) {
        let app = self.state.read(cx);
        let connection = app.connection_id();
        let Some(client) = app.client_handle() else {
            return;
        };
        if self.projects.connection != connection || self.projects.environment != self.project_environment(cx).unwrap_or_default() {
            self.projects = Projects {
                connection,
                environment: self.project_environment(cx).unwrap_or_default(),
                ..Default::default()
            };
        }
        let mut grants = client.session_state();
        self.projects.grants = Some(cx.spawn(async move |this, cx| {
            loop {
                let _ = this.update(cx, |this, cx| {
                    if !this.projects_current(cx) {
                        return;
                    }
                    let (allowed, can_read) = {
                        let g = grants.borrow_and_update();
                        (g.as_ref().is_some_and(|s| s.grants(ORCHESTRATION_OPERATE_SCOPE)), g.as_ref().is_some_and(|s| s.grants("orchestration:read")))
                    };
                    if (!allowed && (this.projects.busy || this.projects.landing.is_some())) || (!can_read && this.projects.pending.is_some()) {
                        this.projects.demand.cancel();
                        this.projects.pending = None;
                        this.projects.busy = false;
                        this.projects.retry_after_scan = false;
                        this.projects.landing = None;
                        this.projects.landing_timeout = None;
                        this.projects.error = Some(model::PERMISSION_MESSAGE.into());
                    }
                    if this.projects.scan.is_none() && this.projects.pending.is_none() && this.projects_access("orchestration:read", cx) {
                        this.scan_projects(cx);
                    }
                    cx.notify();
                });
                if grants.changed().await.is_err() {
                    break;
                }
            }
        }));
        self.scan_projects(cx);
    }
    fn scan_projects(&mut self, cx: &mut Context<Self>) {
        if !self.projects_access("orchestration:read", cx) {
            return;
        }
        self.projects.pending = None;
        self.projects.error = None;
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = fixture_scan() {
            match fixture.get("state").and_then(Value::as_str) {
                Some("error") => self.projects.error = Some("Could not check projects. This server does not support history scanning.".into()),
                Some("pending") => {
                    self.projects.pending = Some(cx.spawn(async move |_, cx| {
                        cx.background_executor().timer(Duration::from_secs(30)).await;
                    }))
                }
                _ => {
                    if let Ok(scan) = serde_json::from_value(fixture.clone()) {
                        self.receive_scan(scan, cx);
                    }
                }
            }
            if let Some(result) = fixture.get("importResult").and_then(|r| serde_json::from_value::<model::ImportResult>(r.clone()).ok()) {
                self.projects.progress.imported = result.imported_count;
                self.projects.progress.skipped = result.skipped_count;
                self.projects.error = self.projects.progress.warning();
            }
            cx.notify();
            return;
        }
        self.project_request::<model::Scan>(model::SCAN, json!({}), cx, |this, result, cx| match result {
            Ok(scan) => this.receive_scan(scan, cx),
            Err(error) => {
                this.projects.error = Some(format!("Could not check projects. {error}"));
                cx.notify();
            }
        });
    }
    fn receive_scan(&mut self, scan: model::Scan, cx: &mut Context<Self>) {
        if self.projects.scan.is_none() {
            self.projects.selected = model::default_selection(&scan, &self.projects.environment, parse_iso(&super::now_iso()).unwrap_or(0));
            self.projects.collapsed.insert("other".into());
        }
        self.projects.scan = Some(scan);
        self.projects.rebuild();
        if std::mem::take(&mut self.projects.retry_after_scan) {
            self.run_import(cx);
        }
        cx.notify();
    }
    /// Own the unary future directly: dropping the step drops its receiver and interrupts RPC.
    fn project_request<T: serde::de::DeserializeOwned + Send + 'static>(&mut self, method: &'static str, payload: Value, cx: &mut Context<Self>, done: impl FnOnce(&mut Self, Result<T, RequestFailure>, &mut Context<Self>) + 'static) {
        let Some(client) = self.state.read(cx).client_handle() else {
            return;
        };
        let Some(connection) = self.projects.connection else { return };
        let token = self.projects.demand.begin(connection);
        let request = client.request(method, payload);
        let deadline = cx.background_executor().timer(Duration::from_secs(30));
        self.projects.pending = Some(cx.spawn(async move |this, cx| {
            let result = {
                futures::pin_mut!(request, deadline);
                match futures::future::select(request, deadline).await {
                    futures::future::Either::Left((reply, _)) => reply.map_err(RequestFailure::from),
                    futures::future::Either::Right(_) => Err(RequestFailure {
                        message: "The server did not answer. Retry or continue without importing.".into(),
                        definitive: false,
                    }),
                }
            };
            let result = cx
                .background_executor()
                .spawn(async move { result.and_then(|value| serde_json::from_value::<T>(value).map_err(|e| RequestFailure { message: e.to_string(), definitive: false })) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let scope = if method == model::SCAN { "orchestration:read" } else { ORCHESTRATION_OPERATE_SCOPE };
                if !this.projects.demand.accepts(token, this.state.read(cx).connection_id(), this.projects_access(scope, cx)) {
                    return;
                }
                this.projects.pending = None;
                done(this, result, cx);
            });
        }));
    }
    fn run_import(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if std::env::var_os("T3_NATIVE_ONBOARDING_SCAN_FIXTURE").is_some() {
            self.projects.error = Some("Snapshot fixtures cannot write projects or import history.".into());
            cx.notify();
            return;
        }
        if self.projects.busy || !self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
            return;
        }
        self.projects.progress.selection = self
            .projects
            .scan
            .as_ref()
            .map(|scan| {
                scan.candidates
                    .iter()
                    .filter_map(|c| {
                        let key = model::key(&self.projects.environment, &c.path);
                        self.projects.selected.contains(&key).then_some(key)
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.projects.progress.skipped = 0;
        self.projects.progress.failures = 0;
        self.projects.error = None;
        self.projects.busy = true;
        self.next_import(0, cx);
    }
    fn next_import(&mut self, offset: usize, cx: &mut Context<Self>) {
        if !self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
            self.projects.busy = false;
            self.projects.error = Some(model::PERMISSION_MESSAGE.into());
            cx.notify();
            return;
        }
        let selection = &self.projects.progress.selection;
        let next = (offset..selection.len()).find(|&i| !self.projects.progress.completed.contains_key(&selection[i]));
        let Some(i) = next else {
            self.projects.busy = false;
            if self.projects.progress.failures > 0 || self.projects.progress.skipped > 0 {
                self.projects.error = self.projects.progress.warning();
                cx.notify();
            } else {
                self.finish_import(cx);
            }
            return;
        };
        let key = selection[i].clone();
        let candidate = self.projects.scan.as_ref().and_then(|scan| scan.candidates.iter().find(|c| model::key(&self.projects.environment, &c.path) == key)).cloned();
        let Some(candidate) = candidate else {
            self.projects.progress.failures += 1;
            self.next_import(i + 1, cx);
            return;
        };
        let app = self.state.read(cx);
        let projects = app.shell.snapshot.as_ref().map_or(&[][..], |s| s.projects.as_slice());
        let existing = model::resolve_project(&candidate, &self.projects.environment, &self.projects.environment, projects);
        let (id, create) = match existing {
            Some(id) => (id, false),
            None => (self.projects.progress.attempts.entry(key.clone()).or_insert_with(uuid).clone(), true),
        };
        if create {
            self.project_request::<Value>(PROJECTS_MUTATE, model::create_payload(&id, &candidate), cx, move |this, result, cx| {
                if let Err(error) = result {
                    this.projects.progress.failed_attempt(&key, error.definitive);
                    this.next_import(i + 1, cx);
                } else {
                    this.import_history(i, key, id, candidate, cx);
                }
            });
        } else {
            self.import_history(i, key, id, candidate, cx);
        }
        cx.notify();
    }
    fn import_history(&mut self, i: usize, key: String, id: String, candidate: model::Candidate, cx: &mut Context<Self>) {
        if !self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
            self.projects.busy = false;
            self.projects.error = Some(model::PERMISSION_MESSAGE.into());
            cx.notify();
            return;
        }
        self.project_request::<model::ImportResult>(model::IMPORT, model::import_payload(&id, &candidate), cx, move |this, result, cx| {
            if !this.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
                this.projects.busy = false;
                this.projects.error = Some(model::PERMISSION_MESSAGE.into());
                cx.notify();
                return;
            }
            match result {
                Ok(result) => this.projects.progress.record(key, id, result),
                Err(error) => {
                    tracing::warn!(%error,"history import failed");
                    this.projects.progress.failed_attempt(&key, error.definitive);
                }
            }
            this.next_import(i + 1, cx);
        });
    }
    fn finish_import(&mut self, cx: &mut Context<Self>) {
        if !self.projects_current(cx) {
            return;
        }
        if !self.projects.progress.selection.is_empty() && !self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
            self.projects.error = Some(model::PERMISSION_MESSAGE.into());
            cx.notify();
            return;
        }
        self.projects.landing = self.projects.progress.landing().map(str::to_owned);
        if self.projects.landing.is_none() {
            self.finish_with_import_notice(cx);
            return;
        }
        self.projects.busy = true;
        self.projects.landing_timeout = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let _ = this.update(cx, |this, cx| {
                if this.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
                    this.projects.landing = None;
                    this.finish_with_import_notice(cx);
                }
            });
        }));
        self.try_import_landing(cx);
    }
    fn try_import_landing(&mut self, cx: &mut Context<Self>) {
        if !self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx) {
            return;
        }
        if let Some(id) = self.projects.landing.clone().filter(|id| self.state.read(cx).shell.project(id).is_some()) {
            self.projects.landing = None;
            self.state.update(cx, |state, cx| state.start_draft(&id, cx));
            self.finish_with_import_notice(cx);
        }
    }
    fn finish_with_import_notice(&mut self, cx: &mut Context<Self>) {
        let progress = &self.projects.progress;
        let notice = progress
            .warning()
            .map(|warning| Toast::new(ToastKind::Warning, "Some history was not imported").description(warning))
            .or_else(|| (progress.imported > 0).then(|| Toast::new(ToastKind::Success, format!("Imported {} {}", progress.imported, if progress.imported == 1 { "thread" } else { "threads" }))));
        self.finish(cx);
        if let Some(notice) = notice {
            self.state.update(cx, |state, cx| state.show_toast(notice, cx));
        }
    }
    fn project_row(&self, row: &Row, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(scan) = &self.projects.scan else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let (indices, label, nested, group, threads) = match row {
            Row::Candidate { index, label, nested } => (vec![*index], label.clone(), *nested, None, scan.candidates[*index].thread_count),
            Row::Group { key, label, indices, threads } => (indices.clone(), Some(label.clone()), false, Some(key.clone()), *threads),
        };
        let keys: Vec<_> = indices.iter().map(|&i| model::key(&self.projects.environment, &scan.candidates[i].path)).collect();
        let checked = keys.iter().filter(|k| self.projects.selected.contains(*k)).count();
        let all = checked == keys.len();
        let id = ElementId::from(format!("welcome-project-check-{}", keys[0]));
        let checkbox = div()
            .id(id.clone())
            .size(r(16.))
            .flex_none()
            .border_1()
            .rounded(r(4.))
            .border_color(hsla(if checked > 0 { p.primary } else { p.input }))
            .bg(hsla(if checked > 0 { p.primary } else { p.background }))
            .flex()
            .items_center()
            .justify_center()
            .when(checked > 0, |d| d.child(if all { Icon::Check } else { Icon::Minus }.render(gpui::px(12.), hsla(p.primary_foreground))));
        let busy = self.projects.busy;
        let toggle = Rc::new(cx.listener(move |this, _: &(), _, cx| {
            if this.projects.busy {
                return;
            }
            for key in &keys {
                if all {
                    this.projects.selected.remove(key);
                } else {
                    this.projects.selected.insert(key.clone());
                }
            }
            cx.notify();
        }));
        let candidate_row = group.is_none();
        let other = group.as_deref() == Some("other");
        let checkbox = if busy {
            checkbox.opacity(0.5)
        } else if candidate_row {
            checkbox
        } else {
            key_stop(
                checkbox,
                &id,
                {
                    let toggle = toggle.clone();
                    move |e, w, cx| toggle(e, w, cx)
                },
                window,
                cx,
            )
        };
        let content = if let Some(group) = group {
            let open = !self.projects.collapsed.contains(&group);
            let id = ElementId::from(format!("welcome-project-expand-{group}"));
            let trigger = h_flex()
                .id(id.clone())
                .flex_1()
                .min_w_0()
                .gap(r(6.))
                .child(if open { Icon::ChevronDown } else { Icon::ChevronRight }.render(gpui::px(14.), hsla(p.muted_foreground)))
                .child(div().truncate().child(label.unwrap_or_default()));
            key_stop(
                trigger,
                &id,
                cx.listener(move |this, _: &(), _, cx| {
                    if !this.projects.collapsed.remove(&group) {
                        this.projects.collapsed.insert(group.clone());
                    }
                    this.projects.rebuild();
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element()
        } else {
            let c = &scan.candidates[indices[0]];
            let (name, parent) = model::split_path(&c.path);
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(r(8.))
                .children(label.map(|label| div().truncate().child(label)))
                .child(
                    h_flex()
                        .min_w_0()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(div().truncate().child(parent.to_owned()))
                        .child(div().flex_none().child(name.to_owned())),
                )
                .into_any_element()
        };
        let sources = indices.iter().flat_map(|&i| scan.candidates[i].sources.iter()).collect::<HashSet<_>>();
        let age = indices
            .iter()
            .filter_map(|&i| scan.candidates[i].last_active_at.as_deref())
            .max()
            .and_then(parse_iso)
            .map(|at| t3_core::sidebar::time::compact_relative_label(at, parse_iso(&super::now_iso()).unwrap_or(0)))
            .unwrap_or_default();
        let meta = h_flex()
            .flex_none()
            .gap(r(4.))
            .items_center()
            .children([("claudeAgent", "Claude Code"), ("codex", "Codex"), ("pi", "Pi")].into_iter().map(|(driver, name)| {
                div()
                    .w(r(16.))
                    .children((!nested && !other && sources.iter().any(|s| s.as_str() == driver)).then(|| ProviderIcon::new(ProviderMark::configured(driver, name, None)).size(gpui::px(12.))))
            }))
            .child(div().w(r(if other { 84. } else { 40. })).text_right().child(if other {
                format!("{} {}", indices.len(), if indices.len() == 1 { "folder" } else { "folders" })
            } else {
                threads.to_string()
            }))
            .child(div().w(r(36.)).text_right().child(if other { String::new() } else { age }));
        let row = h_flex()
            .id(id.clone())
            .w_full()
            .h(r(32.))
            .items_center()
            .gap(r(10.))
            .px(r(8.))
            .when(nested, |d| d.pl(r(32.)))
            .rounded(r(6.))
            .text_sm()
            .hover(|s| s.bg(hsla(p.muted).opacity(0.4)))
            .child(checkbox)
            .child(content)
            .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(meta));
        let row = row.when(candidate_row, |row| row.tooltip(crate::settings::controls::tooltip(scan.candidates[indices[0]].path.clone().into())));
        if candidate_row && !busy {
            key_stop(row, &id, move |e, w, cx| toggle(e, w, cx), window, cx).into_any_element()
        } else {
            row.into_any_element()
        }
    }

    pub(super) fn render_projects(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let busy = self.projects.busy;
        let pending = self.projects.pending.is_some() && self.projects.scan.is_none();
        let can_read = self.projects_access("orchestration:read", cx);
        let allowed = self.projects_access(ORCHESTRATION_OPERATE_SCOPE, cx);
        let count = self.projects.scan.as_ref().map_or(0, |s| s.candidates.len());
        let selected = self
            .projects
            .scan
            .as_ref()
            .map_or(0, |s| s.candidates.iter().filter(|c| self.projects.selected.contains(&model::key(&self.projects.environment, &c.path))).count());
        let rows = self.projects.rows.clone();
        let list = uniform_list(
            "welcome-projects-list",
            rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| range.map(|i| this.project_row(&rows[i], window, cx)).collect::<Vec<_>>()),
        )
        .w_full()
        .h(r((self.projects.rows.len() as f32 * 32.).clamp(32., 320.)));
        let error = if !allowed {
            Some(model::PERMISSION_MESSAGE.to_owned())
        } else {
            self.projects.error.clone().filter(|e| e != model::PERMISSION_MESSAGE)
        };
        v_flex()
            .child(step_title(
                if pending { "Your projects" } else { "Import your projects" },
                "Import projects and conversations from Claude Code and Codex on your selected computers.",
                cx,
            ))
            .when(pending, |d| d.child(div().py(r(24.)).text_sm().text_color(hsla(p.muted_foreground)).child("Looking for projects from Claude Code and Codex…")))
            .when(!can_read && self.projects.scan.is_none(), |d| d.child(div().py(r(16.)).text_sm().child("Waiting for permission to check projects…")))
            .when(count > 0, |d| {
                d.child(
                    h_flex()
                        .mt(r(20.))
                        .items_center()
                        .justify_between()
                        .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!("{selected} of {count} selected")))
                        .child(
                            h_flex()
                                .gap(r(4.))
                                .child(
                                    Button::new("welcome-projects-all", ButtonVariant::Ghost, ButtonSize::Xs)
                                        .label("Select all")
                                        .disabled(busy || selected == count)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(scan) = &this.projects.scan {
                                                this.projects.selected = scan.candidates.iter().map(|c| model::key(&this.projects.environment, &c.path)).collect();
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("welcome-projects-none", ButtonVariant::Ghost, ButtonSize::Xs)
                                        .label("Select none")
                                        .disabled(busy || selected == 0)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.projects.selected.clear();
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .when(self.projects.scan.is_some() && count == 0, |d| {
                d.child(div().py(r(16.)).text_sm().text_color(hsla(p.muted_foreground)).child("No existing Claude Code or Codex projects found."))
            })
            .when(self.projects.scan.as_ref().is_some_and(|s| s.truncated), |d| {
                d.child(div().mt(r(8.)).text_xs().text_color(hsla(p.muted_foreground)).child(model::LIMIT_MESSAGE))
            })
            .when(count > 0, |d| d.child(div().mt(r(8.)).child(list)))
            .children(error.map(|error| div().mt(r(12.)).text_sm().text_color(hsla(p.destructive)).child(error)))
            .when(self.projects.error.is_some() && !busy, |d| {
                d.child(
                    Button::new("welcome-projects-retry", ButtonVariant::Ghost, ButtonSize::Sm)
                        .label("Retry")
                        .disabled(!can_read)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.projects.scan.is_some() {
                                this.projects.retry_after_scan = true;
                                this.scan_projects(cx);
                            } else {
                                this.scan_projects(cx);
                            }
                        })),
                )
            })
            .when(busy, |d| {
                d.child(div().mt(r(12.)).text_sm().text_color(hsla(p.muted_foreground)).child(format!(
                    "Importing… {} projects completed · {} threads imported · {} skipped",
                    self.projects.progress.completed.len(),
                    self.projects.progress.imported,
                    self.projects.progress.skipped
                )))
            })
            .child(
                h_flex()
                    .mt(r(24.))
                    .justify_end()
                    .gap(r(12.))
                    .child(
                        Button::new("welcome-projects-skip", ButtonVariant::GhostMuted, ButtonSize::Default)
                            .label("Do not import projects")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.finish_import(cx))),
                    )
                    .child(
                        Button::new("welcome-projects-import", ButtonVariant::Default, ButtonSize::Default)
                            .label(if busy {
                                "Importing…".into()
                            } else {
                                format!("Import {selected} {}", if selected == 1 { "project" } else { "projects" })
                            })
                            .disabled(!allowed || busy || selected == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.run_import(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(feature = "snapshot")]
fn fixture_scan() -> Option<Value> {
    let path = std::env::var_os("T3_NATIVE_ONBOARDING_SCAN_FIXTURE")?;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
