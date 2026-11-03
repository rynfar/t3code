//! Scheduled tasks: `ScheduledTasksSettings` in `web/src/components/settings/ScheduledTasksSettings.tsx`
//! for the connected environment. The list follows `scheduledTasks.subscribe`; rows enable,
//! run now, edit and delete. The editor opens in the page rather than in a dialog over the
//! settings modal. Webhook signatures, filters, dedupe keys and the deliveries log stay on the
//! web: a webhook task saves its stored webhook settings back unchanged.

use std::collections::HashSet;

use chrono::{Local, NaiveDateTime, TimeZone, Timelike};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::server_config::ServerConfig;
use t3_core::settings::scheduled_tasks::{
    self, DAY_TOGGLES, Draft, Invalid, RunStatus, ScheduleMode, ScheduledTask, WorkspaceMode, method, webhook_address,
};

use super::controls::{Button, ButtonSize, ButtonVariant, Row, Select, SelectOption, Switch, group, muted_text, r, section_title};
use super::providers::{Stream, failure_text};
use super::source_control::{Tone, tone_badge};
use super::threads::{model_options, parse_selection, selection_label, selection_value};
use super::{Block, SettingsView, store};
use crate::composer::controls::floating_shadow;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const LOCAL_FORMAT: &str = "%Y-%m-%d %H:%M";

/// `Intl.DateTimeFormat` medium date, short time.
fn once_label(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|time| time.format("%b %-d, %Y, %-I:%M %p").to_string()).unwrap_or_default()
}

fn local_field(iso: &str) -> Option<String> {
    let ms = t3_core::sidebar::time::parse_iso(iso)?;
    Some(Local.timestamp_millis_opt(ms).single()?.format(LOCAL_FORMAT).to_string())
}

/// The local field's time as `toISOString()`; `None` for a time the zone skips.
fn field_to_iso(text: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(&text.replace('T', " "), LOCAL_FORMAT).ok()?;
    let local = Local.from_local_datetime(&naive).single()?;
    Some(local.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

fn next_full_hour() -> String {
    let now = Local::now();
    let next = now + chrono::Duration::minutes(i64::from(60 - now.minute()));
    next.with_second(0).and_then(|time| time.with_nanosecond(0)).unwrap_or(next).format(LOCAL_FORMAT).to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The editor's text fields, created once and filled from a draft when it opens.
pub(crate) struct EditorInputs {
    title: Entity<InputState>,
    prompt: Entity<TextareaState>,
    time_of_day: Entity<InputState>,
    run_at: Entity<InputState>,
    interval: Entity<InputState>,
    base_ref: Entity<InputState>,
    checkout: Entity<InputState>,
}

pub(crate) struct ScheduledTasksState {
    tasks: Option<Result<Vec<ScheduledTask>, String>>,
    pub(super) stream: Option<(u64, Stream)>,
    busy: HashSet<String>,
    menu: Option<String>,
    editor: Option<Draft>,
    editor_error: Option<Invalid>,
    saving: bool,
    save_error: Option<String>,
    inputs: EditorInputs,
}

impl ScheduledTasksState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<SettingsView>| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let inputs = EditorInputs {
            title: input("e.g. Check for Sentry issues", window, cx),
            prompt: cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 10).placeholder("What should the agent do each time this runs?")),
            time_of_day: input("09:00", window, cx),
            run_at: input("YYYY-MM-DD HH:MM", window, cx),
            interval: input("15", window, cx),
            base_ref: input("main", window, cx),
            checkout: input("/path/to/checkout", window, cx),
        };
        ScheduledTasksState { tasks: None, stream: None, busy: HashSet::new(), menu: None, editor: None, editor_error: None, saving: false, save_error: None, inputs }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        let inputs = &self.inputs;
        [&inputs.title, &inputs.time_of_day, &inputs.run_at, &inputs.interval, &inputs.base_ref, &inputs.checkout].into_iter()
    }

    pub(super) fn textarea(&self) -> &Entity<TextareaState> {
        &self.inputs.prompt
    }
}

/// A project the editor offers: open ones, pinned first, plus the selected one if archived.
fn project_options(config_projects: &[t3_core::shell::ProjectShell], selected: &str) -> Vec<SelectOption> {
    let mut projects: Vec<_> = config_projects.iter().filter(|project| project.archived_at.is_none() || project.id == selected).collect();
    projects.sort_by_key(|project| project.pinned_at.is_none());
    projects.into_iter().map(|project| SelectOption::new(project.id.clone(), project.title.clone())).collect()
}

impl SettingsView {
    fn webhook_denial(&self, cx: &App) -> Option<String> {
        t3_core::managed_policy::capability_denial(self.state.read(cx).server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()), t3_core::managed_policy::Capability::WebhookTasks)
    }

    fn shell_projects(&self, cx: &App) -> Vec<t3_core::shell::ProjectShell> {
        self.state.read(cx).shell.snapshot.as_ref().map(|snapshot| snapshot.projects.clone()).unwrap_or_default()
    }

    /// Follows `scheduledTasks.subscribe` while the page is open on a connection.
    fn ensure_task_stream(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.state.read(cx).connection_id() else {
            self.scheduled.stream = None;
            return;
        };
        if self.scheduled.stream.as_ref().is_some_and(|(id, _)| *id == connection) {
            return;
        }
        self.scheduled.tasks = None;
        let stream = self.subscribe(method::SUBSCRIBE, serde_json::json!({}), cx, |this, item, _| {
            this.scheduled.tasks = Some(match item {
                Ok(value) => match scheduled_tasks::decode_list(&value) {
                    Some(tasks) => Ok(tasks),
                    None => return,
                },
                Err(message) => Err(message),
            });
        });
        self.scheduled.stream = stream.map(|stream| (connection, stream));
    }

    fn task_rpc(&mut self, id: &str, rpc: &'static str, payload: Value, cx: &mut Context<Self>) {
        if rpc == method::SET_ENABLED && payload["enabled"] == true && self.webhook_denial(cx).is_some()
            && self.scheduled.tasks.as_ref().and_then(|tasks| tasks.as_ref().ok()).is_some_and(|tasks| tasks.iter().any(|task| task.id == id && task.is_webhook())) { return; }
        let task = self.state.read(cx).rpc(rpc, payload);
        let id = id.to_owned();
        self.scheduled.busy.insert(id.clone());
        self.scheduled.menu = None;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.scheduled.busy.remove(&id);
                if let Err(error) = result {
                    store::show_error(format!("Could not update scheduled task: {}", failure_text(&error, "Try again.")), cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_editor(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = &self.scheduled.inputs;
        let set = |input: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        set(&inputs.title.clone(), &draft.title, window, cx);
        set(&inputs.time_of_day.clone(), &draft.time_of_day, window, cx);
        set(&inputs.run_at.clone(), &draft.run_at, window, cx);
        set(&inputs.interval.clone(), &draft.interval_minutes, window, cx);
        set(&inputs.base_ref.clone(), &draft.base_ref, window, cx);
        set(&inputs.checkout.clone(), &draft.existing_worktree_path, window, cx);
        let prompt = draft.prompt.clone();
        self.scheduled.inputs.prompt.clone().update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.scheduled.editor = Some(draft);
        self.scheduled.editor_error = None;
        self.scheduled.save_error = None;
        self.scheduled.menu = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    fn new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let projects = self.shell_projects(cx);
        let project = projects
            .iter()
            .find(|project| project.archived_at.is_none() && project.pinned_at.is_some())
            .or_else(|| projects.iter().find(|project| project.archived_at.is_none()));
        let mut draft = Draft { project_id: project.map(|project| project.id.clone()).unwrap_or_default(), ..Draft::default() };
        draft.model = self.default_task_model(project, cx);
        self.open_editor(draft, window, cx);
    }

    /// The project's default model, then the environment's, then the first usable provider's.
    fn default_task_model(&self, project: Option<&t3_core::shell::ProjectShell>, cx: &App) -> Option<ModelSelection> {
        let config = self.thread_settings(cx)?;
        project
            .and_then(|project| project.default_model_selection.clone())
            .or_else(|| config.settings.default_model_selection.clone())
            .or_else(|| providers::project_default_selection(&providers::entries(config, None)))
    }

    /// Reads the text fields into the draft, the way the web's controlled inputs hold them.
    fn read_editor_inputs(&mut self, cx: &App) {
        let inputs = &self.scheduled.inputs;
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let (title, time, run_at, interval, base_ref, checkout) =
            (value(&inputs.title), value(&inputs.time_of_day), value(&inputs.run_at), value(&inputs.interval), value(&inputs.base_ref), value(&inputs.checkout));
        let prompt = inputs.prompt.read(cx).value().to_string();
        if let Some(draft) = self.scheduled.editor.as_mut() {
            draft.title = title;
            draft.prompt = prompt;
            draft.time_of_day = time;
            draft.run_at = run_at;
            draft.interval_minutes = interval;
            draft.base_ref = base_ref;
            draft.existing_worktree_path = checkout;
        }
    }

    fn edit_draft(&mut self, edit: impl FnOnce(&mut Draft), cx: &mut Context<Self>) {
        self.read_editor_inputs(cx);
        if let Some(draft) = self.scheduled.editor.as_mut() {
            edit(draft);
        }
        cx.notify();
    }

    fn save_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scheduled.saving {
            return;
        }
        self.read_editor_inputs(cx);
        let Some(config) = self.thread_settings(cx) else { return };
        let once = config.environment.capabilities.one_time_schedules == Some(true);
        let ceiling = scheduled_tasks::max_runtime_mode(config.managed_policy.as_ref()).map(str::to_owned);
        let Some(draft) = self.scheduled.editor.clone() else { return };
        if draft.enabled && draft.schedule_mode == ScheduleMode::Webhook && self.webhook_denial(cx).is_some() { return; }
        let input = match draft.upsert_input(once, ceiling.as_deref(), field_to_iso) {
            Ok(input) => input,
            Err(invalid) => {
                self.scheduled.editor_error = Some(invalid);
                return cx.notify();
            }
        };
        self.scheduled.editor_error = None;
        self.scheduled.save_error = None;
        self.scheduled.saving = true;
        let task = self.state.read(cx).rpc(method::UPSERT, input);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.scheduled.saving = false;
                match result {
                    Ok(_) => this.scheduled.editor = None,
                    Err(error) => this.scheduled.save_error = Some(failure_text(&error, "Try again.")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn scheduled_tasks_page(&mut self, cx: &mut Context<Self>) -> Vec<Block> {
        self.ensure_task_stream(cx);
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], self.task_empty("No environments available", "Connect an environment to manage scheduled tasks.", cx))];
        };
        if self.scheduled.editor.is_some() {
            return vec![Block::new(&[], self.task_editor(&config, cx))];
        }
        let new_task = Button::new("new-scheduled-task", ButtonVariant::Outline, ButtonSize::Sm)
            .icon(Icon::Plus)
            .label("New task")
            .on_click(cx.listener(|this, _, window, cx| this.new_task(window, cx)));
        let header = h_flex().justify_between().pr(r(8.)).child(section_title("Scheduled tasks")).child(new_task);
        let body = match &self.scheduled.tasks {
            None => self.task_empty("Loading scheduled tasks\u{2026}", "", cx),
            Some(Err(error)) => self.task_empty("Could not load scheduled tasks", &error.clone(), cx),
            Some(Ok(tasks)) if tasks.is_empty() => self.task_empty("No scheduled tasks", "No tasks match this environment and project selection.", cx),
            Some(Ok(tasks)) => {
                let tasks = tasks.clone();
                let now = now_ms();
                group(tasks.iter().map(|task| self.task_row(task, now, cx)).collect(), cx).into_any_element()
            }
        };
        vec![Block::new(&[], v_flex().gap(r(10.)).child(header).child(body))]
    }

    fn task_empty(&self, title: &str, description: &str, cx: &App) -> AnyElement {
        let p = palette(cx);
        v_flex()
            .items_center()
            .gap(r(6.))
            .px(r(16.))
            .py(r(32.))
            .rounded(r(12.))
            .border_1()
            .border_color(hsla(p.border).opacity(0.6))
            .child(crate::settings::controls::icon(Icon::CalendarClock, 20., hsla(p.muted_foreground)))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title.to_owned()))
            .when(!description.is_empty(), |this| this.child(div().text_xs().text_center().text_color(hsla(p.muted_foreground)).child(description.to_owned())))
            .into_any_element()
    }

    fn task_row(&self, task: &ScheduledTask, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let busy = self.scheduled.busy.contains(&task.id);
        let status = format!("{} \u{00b7} {}", task.schedule.label(once_label), task.state_label(now));
        let badge = task.last_run_status.badge().map(|text| {
            let tone = match task.last_run_status {
                RunStatus::Failed => Tone::Error,
                RunStatus::Succeeded => Tone::Success,
                _ => Tone::Info,
            };
            tone_badge(text, tone, cx)
        });
        let (id, enabled, view) = (task.id.clone(), task.enabled, cx.entity().downgrade());
        let denial = (!task.enabled && task.is_webhook()).then(|| self.webhook_denial(cx)).flatten();
        let toggle = crate::components::managed::ManagedControl::new(SharedString::from(format!("managed-task-enabled-{}", task.id)), denial, Switch::new(SharedString::from(format!("scheduled-task-enabled-{}", task.id)), task.enabled).disabled(busy).on_change(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| this.task_rpc(&id, method::SET_ENABLED, scheduled_tasks::set_enabled_payload(&id, !enabled), cx));
        }));
        h_flex()
            .px(r(16.))
            .py(r(12.))
            .gap(r(16.))
            .items_start()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(r(4.))
                    .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(task.title.clone()))
                    .child(div().text_xs().line_clamp(2).text_color(hsla(p.muted_foreground)).child(task.prompt.clone()))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(r(6.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(status)
                            .children(badge)
                            .children(task.last_run_error.clone().map(|error| div().text_color(hsla(p.destructive_foreground)).child(error))),
                    ),
            )
            .child(h_flex().flex_none().gap(r(8.)).child(self.task_menu(task, busy, cx)).child(toggle))
            .into_any_element()
    }

    /// The row's "…" menu: Edit, Run now (not for webhook tasks), Delete.
    fn task_menu(&self, task: &ScheduledTask, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let open = self.scheduled.menu.as_deref() == Some(task.id.as_str());
        let id = task.id.clone();
        let trigger = Button::new(SharedString::from(format!("scheduled-task-menu-{}", task.id)), ButtonVariant::GhostMuted, ButtonSize::IconSm)
            .icon(Icon::Ellipsis)
            .tooltip(format!("Actions for {}", task.title))
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.scheduled.menu = if open { None } else { Some(id.clone()) };
                cx.notify();
            }));
        let popup = open.then(|| {
            let item = |label: &'static str, destructive: bool| {
                h_flex()
                    .id(label)
                    .min_h(r(28.))
                    .px(r(8.))
                    .py(r(4.))
                    .rounded(r(4.))
                    .text_sm()
                    .cursor_pointer()
                    .text_color(if destructive { hsla(p.destructive_foreground) } else { hsla(p.foreground) })
                    .hover(|this| this.bg(hsla(p.accent)))
                    .child(label)
            };
            let (edit_task, run_id, delete_id) = (task.clone(), task.id.clone(), task.id.clone());
            let webhook = task.is_webhook();
            div().absolute().bottom_0().right_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
                        v_flex()
                            .occlude()
                            .mt(r(4.))
                            .min_w(r(160.))
                            .p(r(4.))
                            .rounded(r(8.))
                            .border_1()
                            .border_color(hsla(p.dropdown_border))
                            .bg(hsla(p.popover))
                            .shadow(floating_shadow(cx.theme().is_dark()))
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.scheduled.menu = None;
                                cx.notify();
                            }))
                            .child(item("Edit", false).on_click(cx.listener(move |this, _, window, cx| {
                                this.open_editor(Draft::from_task(&edit_task, local_field), window, cx)
                            })))
                            .when(!webhook, |this| {
                                this.child(item("Run now", false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.task_rpc(&run_id, method::RUN_NOW, scheduled_tasks::id_payload(&run_id), cx)
                                })))
                            })
                            .child(div().my(r(4.)).h(px(1.)).bg(hsla(p.border)))
                            .child(item("Delete", true).on_click(cx.listener(move |this, _, _, cx| {
                                this.task_rpc(&delete_id, method::DELETE, scheduled_tasks::id_payload(&delete_id), cx)
                            }))),
                    ),
                )
                .with_priority(2),
            )
        });
        div().relative().flex_none().child(trigger).children(popup).into_any_element()
    }

    fn task_editor(&mut self, config: &ServerConfig, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(draft) = self.scheduled.editor.clone() else { return div().into_any_element() };
        let projects = self.shell_projects(cx);
        let inputs = &self.scheduled.inputs;
        let field = |input: &Entity<InputState>, width: f32| div().w(r(width)).child(Input::new(input).small());
        let once_supported = config.environment.capabilities.one_time_schedules == Some(true);
        let editing = draft.editing_id.is_some();

        let project_state = cx.entity().downgrade();
        let project_select = Select::new("scheduled-task-project", project_options(&projects, &draft.project_id))
            .selected(draft.project_id.clone())
            .label(projects.iter().find(|project| project.id == draft.project_id).map_or_else(|| "Select a project".to_owned(), |project| project.title.clone()))
            .width(240.)
            .on_change(move |value, _, cx| {
                let _ = project_state.update(cx, |this, cx| this.edit_draft(|draft| draft.project_id = value.to_string(), cx));
            });

        let workspace_state = cx.entity().downgrade();
        let workspace = Select::new("scheduled-task-workspace", WorkspaceMode::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect())
            .selected(draft.workspace_mode.as_str())
            .width(240.)
            .on_change(move |value, _, cx| {
                if let Some(mode) = WorkspaceMode::parse(&value) {
                    let _ = workspace_state.update(cx, |this, cx| this.edit_draft(|draft| draft.workspace_mode = mode, cx));
                }
            });
        let origin_state = cx.entity().downgrade();
        let workspace_detail: Option<AnyElement> = match draft.workspace_mode {
            WorkspaceMode::Worktree => Some(
                Row::new("scheduled-task-base-branch", "Base branch")
                    .description("Start from remote")
                    .control(
                        h_flex().gap(r(8.)).child(field(&inputs.base_ref, 160.)).child(Switch::new("scheduled-task-start-from-origin", draft.start_from_origin).on_change(
                            move |on, _, cx| {
                                let _ = origin_state.update(cx, |this, cx| this.edit_draft(|draft| draft.start_from_origin = on, cx));
                            },
                        )),
                    )
                    .into_any_element(),
            ),
            WorkspaceMode::ExistingWorktree => Some(Row::new("scheduled-task-checkout", "Checkout path").control(field(&inputs.checkout, 280.)).into_any_element()),
            WorkspaceMode::Root => None,
        };

        let options = model_options(config, None);
        let model_state = cx.entity().downgrade();
        let model: AnyElement = match &draft.model {
            _ if options.is_empty() => muted_text("No providers available", cx).into_any_element(),
            selection => Select::new("scheduled-task-model", options)
                .selected(selection.as_ref().map(selection_value).unwrap_or_default())
                .label(selection.as_ref().map_or_else(|| "Select a model".to_owned(), |selection| selection_label(config, selection)))
                .width(240.)
                .popup_width(260.)
                .show_check()
                .on_change(move |value, _, cx| {
                    let Some(selection) = parse_selection(&value) else { return };
                    let _ = model_state.update(cx, |this, cx| {
                        this.edit_draft(
                            |draft| {
                                let same = draft.model.as_ref().is_some_and(|current| current.instance_id == selection.instance_id && current.model == selection.model);
                                if !same {
                                    draft.model = Some(selection);
                                }
                            },
                            cx,
                        )
                    });
                })
                .into_any_element(),
        };

        let modes = h_flex().gap(r(4.)).children(ScheduleMode::ALL.into_iter().filter(|mode| *mode != ScheduleMode::Once || once_supported || draft.schedule_mode == ScheduleMode::Once).map(|mode| {
            let selected = draft.schedule_mode == mode;
            let denial = (mode == ScheduleMode::Webhook && !selected).then(|| self.webhook_denial(cx)).flatten();
            crate::components::managed::ManagedControl::new(SharedString::from(format!("managed-task-mode-{}", mode.label())), denial, Button::new(SharedString::from(format!("scheduled-task-mode-{}", mode.label())), if selected { ButtonVariant::Default } else { ButtonVariant::Outline }, ButtonSize::Compact)
                .label(mode.label())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if mode == ScheduleMode::Webhook && this.webhook_denial(cx).is_some() { return; }
                    this.edit_draft(|draft| draft.choose_schedule(mode, next_full_hour), cx);
                    let (prompt, run_at) = this.scheduled.editor.as_ref().map(|draft| (draft.prompt.clone(), draft.run_at.clone())).unwrap_or_default();
                    this.scheduled.inputs.prompt.clone().update(cx, |input, cx| {
                        if input.value().as_ref() != prompt {
                            input.set_value(prompt, window, cx)
                        }
                    });
                    this.scheduled.inputs.run_at.clone().update(cx, |input, cx| {
                        if input.value().as_ref() != run_at {
                            input.set_value(run_at, window, cx)
                        }
                    });
                })))
        }));
        let schedule_detail: AnyElement = match draft.schedule_mode {
            ScheduleMode::Fixed => h_flex()
                .gap(r(8.))
                .text_sm()
                .child("Run at")
                .child(field(&inputs.time_of_day, 80.))
                .child("on")
                .children(DAY_TOGGLES.into_iter().map(|(day, label)| {
                    let on = draft.weekdays.contains(&day);
                    Button::new(SharedString::from(format!("scheduled-task-day-{day}")), if on { ButtonVariant::Default } else { ButtonVariant::Outline }, ButtonSize::Compact)
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.edit_draft(|draft| draft.toggle_weekday(day), cx)))
                }))
                .into_any_element(),
            ScheduleMode::Once => h_flex().gap(r(8.)).text_sm().child("Run on").child(field(&inputs.run_at, 180.)).into_any_element(),
            ScheduleMode::Interval => h_flex().gap(r(8.)).text_sm().child("Run every").child(field(&inputs.interval, 72.)).child("minutes").into_any_element(),
            ScheduleMode::Webhook => {
                let task = draft.editing_id.as_ref().and_then(|id| self.scheduled.tasks.as_ref()?.as_ref().ok()?.iter().find(|task| &task.id == id).cloned());
                let address = task.and_then(|task| task.webhook).map(|hook| webhook_address(&hook, self.state.read(cx).environment()));
                v_flex()
                    .gap(r(6.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(match address {
                        Some((url, copyable, note)) => v_flex()
                            .gap(r(4.))
                            .child(
                                h_flex()
                                    .gap(r(8.))
                                    .child(div().flex_1().min_w_0().truncate().font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.foreground)).child(url.clone()))
                                    .when(copyable, |this| {
                                        this.child(Button::new("scheduled-task-copy-webhook", ButtonVariant::Outline, ButtonSize::Compact).icon(Icon::Copy).label("Copy").on_click(
                                            move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone())),
                                        ))
                                    }),
                            )
                            .when(!note.is_empty(), |this| this.child(note))
                            .into_any_element(),
                        None => div().child("The URL appears after you save.").into_any_element(),
                    })
                    .child("Each request runs the prompt. Use {{body.path}}, {{headers.name}}, {{query.name}}, {{body}} or {{request}} in the prompt; only what it names reaches the agent.")
                    .into_any_element()
            }
        };

        let enabled_state = cx.entity().downgrade();
        let rows = vec![
            Row::new("scheduled-task-name", "Name").control(field(&inputs.title, 280.)).into_any_element(),
            Row::new("scheduled-task-destination", "Destination")
                .description(if draft.thread_id.is_some() {
                    "Each run continues this conversation, in its workspace and with its current model."
                } else {
                    "Each run starts a new thread with the workspace and model below."
                })
                .into_any_element(),
            Row::new("scheduled-task-project-row", "Project").control(project_select).into_any_element(),
            Row::new("scheduled-task-workspace-row", "Workspace").control(workspace).into_any_element(),
        ]
        .into_iter()
        .chain(workspace_detail)
        .chain([
            Row::new("scheduled-task-prompt", "Prompt").below(div().mt(r(12.)).pb(r(14.)).child(Textarea::new(&inputs.prompt))).into_any_element(),
            Row::new("scheduled-task-model-row", "Model").control(model).into_any_element(),
            Row::new("scheduled-task-schedule", "Schedule").control(modes).below(div().mt(r(12.)).pb(r(14.)).children(self.webhook_denial(cx).map(|reason| crate::components::managed::notice(if draft.schedule_mode == ScheduleMode::Webhook { format!("{reason} Switch this task to a time or an interval, or turn it off, to save it.") } else { reason }, cx))).child(schedule_detail)).into_any_element(),
            Row::new("scheduled-task-enabled", "Enabled")
                .description("Disabled tasks stay saved but do not run.")
                .control(Switch::new("scheduled-task-enabled-switch", draft.enabled).on_change(move |on, _, cx| {
                    let _ = enabled_state.update(cx, |this, cx| this.edit_draft(|draft| draft.enabled = on, cx));
                }))
                .into_any_element(),
        ])
        .collect();

        let error = self.scheduled.editor_error.clone().map(|invalid| (invalid.title.to_owned(), invalid.description.to_owned())).or_else(|| {
            self.scheduled.save_error.clone().map(|message| ("Could not save scheduled task".to_owned(), message))
        });
        let connected = self.state.read(cx).connection_id().is_some();
        v_flex()
            .gap(r(10.))
            .child(
                v_flex()
                    .px(r(16.))
                    .gap(r(4.))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(if editing { "Edit task" } else { "New task" }))
                    .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(
                        "Run a prompt automatically on an interval, at a fixed time, once at a set date, or when a webhook is called.",
                    )),
            )
            .child(group(rows, cx))
            .children(error.map(|(title, description)| {
                v_flex()
                    .px(r(14.))
                    .py(r(10.))
                    .rounded(r(12.))
                    .border_1()
                    .border_color(hsla(p.error).opacity(0.32))
                    .bg(hsla(p.error_surface))
                    .text_sm()
                    .text_color(hsla(p.error_foreground))
                    .child(div().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().opacity(0.8).child(description))
            }))
            .when(!connected, |this| this.child(div().px(r(16.)).child(muted_text("Reconnect this environment before saving.", cx))))
            .child(
                h_flex()
                    .justify_end()
                    .gap(r(8.))
                    .child(Button::new("scheduled-task-cancel", ButtonVariant::Ghost, ButtonSize::Sm).label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.scheduled.editor = None;
                        cx.notify();
                    })))
                    .child(
                        Button::new("scheduled-task-save", ButtonVariant::Default, ButtonSize::Sm)
                            .label(if editing { "Save task" } else { "Create task" })
                            .disabled(self.scheduled.saving || !connected || (draft.enabled && draft.schedule_mode == ScheduleMode::Webhook && self.webhook_denial(cx).is_some()))
                            .on_click(cx.listener(|this, _, window, cx| this.save_task(window, cx))),
                    ),
            )
            .into_any_element()
    }
}
