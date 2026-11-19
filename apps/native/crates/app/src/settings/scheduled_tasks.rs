//! Scheduled tasks: `ScheduledTasksSettings` in `web/src/components/settings/ScheduledTasksSettings.tsx`
//! for the connected environment. Follows `scheduledTasks.subscribe` (including server `timeZone`),
//! supports thread modes (`new_thread` / `continue` / `fresh_session`, `pingdotgg/t3code#30326`),
//! clickable thread links and move-to-thread destination picking, delete confirmation, webhook
//! URL rotation, signature verification, max delivery age, filters, dedupe keys, and the webhook
//! deliveries inspector.

use std::collections::{HashMap, HashSet};

use chrono::{Local, NaiveDateTime, TimeZone, Timelike};
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Sizable, h_flex, v_flex};
use serde_json::Value;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::server_config::ServerConfig;
use t3_core::settings::scheduled_tasks::{
    self, DAY_TOGGLES, Draft, Invalid, MAX_WEBHOOK_FILTERS, RunStatus, ScheduleMode, ScheduledTask, SignatureEncoding, ThreadMode,
    WebhookDelivery, WebhookDeliverySummary, WebhookFilterOp, WorkspaceMode, decode_webhook_deliveries, decode_webhook_delivery,
    destination_threads, get_delivery_payload, list_deliveries_payload, method, schedule_time_zone_label, webhook_address, webhook_delivery_label,
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

fn stop_slash_shortcut(event: &KeyDownEvent, _: &mut Window, cx: &mut App) {
    if event.keystroke.key == "/" && !event.keystroke.modifiers.modified() {
        cx.stop_propagation();
    }
}

fn delivery_outcome_tone(outcome: &str) -> Tone {
    match outcome {
        "accepted" => Tone::Success,
        "duplicate" | "coalesced" | "disabled" | "rate_limited" | "expired" => Tone::Warning,
        "dispatch_failed" | "rejected_signature" => Tone::Error,
        _ => Tone::Secondary,
    }
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
    thread_search: Entity<InputState>,
    max_age: Entity<InputState>,
    replay_key: Entity<InputState>,
    coalesce_key: Entity<InputState>,
    coalesce_window: Entity<InputState>,
    sig_header: Entity<InputState>,
    sig_prefix: Entity<InputState>,
    sig_secret: Entity<InputState>,
}

struct DeliveriesView {
    task_id: String,
    task_title: String,
    loading: bool,
    list: Result<Vec<WebhookDeliverySummary>, String>,
    selected_id: Option<String>,
    loading_detail: bool,
    detail: Option<Result<WebhookDelivery, String>>,
    seq: u64,
}

pub(crate) struct ScheduledTasksState {
    tasks: Option<Result<Vec<ScheduledTask>, String>>,
    time_zone: Option<String>,
    pub(super) stream: Option<(u64, Stream)>,
    busy: HashSet<String>,
    menu: Option<String>,
    editor: Option<Draft>,
    editor_error: Option<Invalid>,
    saving: bool,
    rotating_url: bool,
    save_error: Option<String>,
    pending_task: Option<String>,
    thread_picker_open: bool,
    filter_inputs: HashMap<u64, (Entity<InputState>, Entity<InputState>)>,
    deliveries: Option<DeliveriesView>,
    inputs: EditorInputs,
    _subscriptions: Vec<gpui::Subscription>,
}

impl ScheduledTasksState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<SettingsView>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let thread_search = input("Search threads in this project…", window, cx);
        let subscriptions = vec![cx.subscribe(&thread_search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let inputs = EditorInputs {
            title: input("e.g. Check for Sentry issues", window, cx),
            prompt: cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 10).placeholder("What should the agent do each time this runs?")),
            time_of_day: input("09:00", window, cx),
            run_at: input("YYYY-MM-DD HH:MM", window, cx),
            interval: input("15", window, cx),
            base_ref: input("main", window, cx),
            checkout: input("/path/to/checkout", window, cx),
            thread_search,
            max_age: input("No limit (1–1440)", window, cx),
            replay_key: input("e.g. {{headers.x-github-delivery}}", window, cx),
            coalesce_key: input("e.g. {{body.repository.full_name}}", window, cx),
            coalesce_window: input("120 (10–3600)", window, cx),
            sig_header: input("x-hub-signature-256", window, cx),
            sig_prefix: input("sha256=", window, cx),
            sig_secret: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Signing secret")),
        };
        ScheduledTasksState {
            tasks: None,
            time_zone: None,
            stream: None,
            busy: HashSet::new(),
            menu: None,
            editor: None,
            editor_error: None,
            saving: false,
            rotating_url: false,
            save_error: None,
            pending_task: None,
            thread_picker_open: false,
            filter_inputs: HashMap::new(),
            deliveries: None,
            inputs,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn inputs(&self) -> impl Iterator<Item = &Entity<InputState>> {
        let inputs = &self.inputs;
        [
            &inputs.title,
            &inputs.time_of_day,
            &inputs.run_at,
            &inputs.interval,
            &inputs.base_ref,
            &inputs.checkout,
            &inputs.thread_search,
            &inputs.max_age,
            &inputs.replay_key,
            &inputs.coalesce_key,
            &inputs.coalesce_window,
            &inputs.sig_header,
            &inputs.sig_prefix,
            &inputs.sig_secret,
        ]
        .into_iter()
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
    fn can_operate_tasks(&self, cx: &App) -> bool {
        self.may(ORCHESTRATION_OPERATE_SCOPE, cx)
    }

    fn webhook_denial(&self, cx: &App) -> Option<String> {
        t3_core::managed_policy::capability_denial(
            self.state.read(cx).server_config.config.as_ref().and_then(|c| c.managed_policy.as_ref()),
            t3_core::managed_policy::Capability::WebhookTasks,
        )
    }

    fn shell_projects(&self, cx: &App) -> Vec<t3_core::shell::ProjectShell> {
        self.state.read(cx).shell.snapshot.as_ref().map(|snapshot| snapshot.projects.clone()).unwrap_or_default()
    }

    fn thread_title_for(&self, thread_id: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .shell
            .snapshot
            .as_ref()?
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .map(|thread| thread.title.clone())
    }

    fn open_task_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| state.select_thread(&thread_id, cx));
        window.dispatch_action(Box::new(super::CloseSettings), cx);
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
        self.scheduled.time_zone = None;
        let stream = self.subscribe(method::SUBSCRIBE, serde_json::json!({}), cx, |this, item, _| {
            match item {
                Ok(value) => {
                    if let Some(list) = scheduled_tasks::decode_list_with_timezone(&value) {
                        this.scheduled.time_zone = list.time_zone;
                        this.scheduled.tasks = Some(Ok(list.tasks));
                    }
                }
                Err(message) => {
                    this.scheduled.tasks = Some(Err(message));
                }
            }
        });
        self.scheduled.stream = stream.map(|stream| (connection, stream));
    }

    fn task_rpc(&mut self, id: &str, rpc: &'static str, payload: Value, cx: &mut Context<Self>) {
        if !self.can_operate_tasks(cx) {
            return;
        }
        if rpc == method::SET_ENABLED
            && payload["enabled"] == true
            && self.webhook_denial(cx).is_some()
            && self.scheduled.tasks.as_ref().and_then(|tasks| tasks.as_ref().ok()).is_some_and(|tasks| tasks.iter().any(|task| task.id == id && task.is_webhook()))
        {
            return;
        }
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

    fn confirm_delete_task(&mut self, id: String, title: String, cx: &mut Context<Self>) {
        self.scheduled.menu = None;
        let view = cx.entity().downgrade();
        crate::dialog::ask(
            format!("Delete \"{title}\"?"),
            "This scheduled task will stop running and cannot be recovered.",
            "Delete",
            move |_, cx| {
                let id = id.clone();
                let _ = view.update(cx, |this, cx| {
                    this.task_rpc(&id, method::DELETE, scheduled_tasks::id_payload(&id), cx);
                });
            },
            cx,
        );
    }

    fn confirm_rotate_webhook_url(&mut self, task_id: String, cx: &mut Context<Self>) {
        if self.scheduled.rotating_url || !self.can_operate_tasks(cx) {
            return;
        }
        let view = cx.entity().downgrade();
        crate::dialog::ask(
            "Rotate webhook URL?",
            "Any external service calling the old URL will stop working until you update it with the new URL.",
            "Rotate URL",
            move |_, cx| {
                let task_id = task_id.clone();
                let _ = view.update(cx, |this, cx| {
                    this.scheduled.rotating_url = true;
                    cx.notify();
                    let rpc = this.state.read(cx).rpc(method::ROTATE_WEBHOOK_TOKEN, scheduled_tasks::id_payload(&task_id));
                    cx.spawn(async move |this, cx| {
                        let result = rpc.await;
                        let _ = this.update(cx, |this, cx| {
                            this.scheduled.rotating_url = false;
                            if let Err(error) = result {
                                store::show_error(format!("Could not rotate webhook URL: {}", failure_text(&error, "Try again.")), cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                });
            },
            cx,
        );
    }

    fn open_deliveries(&mut self, task_id: String, task_title: String, cx: &mut Context<Self>) {
        self.scheduled.menu = None;
        self.scheduled.editor = None;
        self.scheduled.deliveries = Some(DeliveriesView {
            task_id: task_id.clone(),
            task_title,
            loading: true,
            list: Ok(Vec::new()),
            selected_id: None,
            loading_detail: false,
            detail: None,
            seq: 0,
        });
        self.refresh_deliveries(cx);
        cx.notify();
    }

    fn refresh_deliveries(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.scheduled.deliveries.as_mut() else { return };
        view.seq += 1;
        let seq = view.seq;
        let task_id = view.task_id.clone();
        view.loading = true;
        let Some(rpc) = self
            .state
            .read(cx)
            .request(method::LIST_WEBHOOK_DELIVERIES, list_deliveries_payload(&task_id), |reply| decode_webhook_deliveries(&reply))
        else {
            view.loading = false;
            view.list = Err("Not connected to server.".into());
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = rpc.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                if let Some(view) = this.scheduled.deliveries.as_mut()
                    && view.seq == seq
                {
                    view.loading = false;
                    view.list = result;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn select_delivery(&mut self, delivery_id: String, cx: &mut Context<Self>) {
        let Some(view) = self.scheduled.deliveries.as_mut() else { return };
        view.seq += 1;
        let seq = view.seq;
        let task_id = view.task_id.clone();
        view.selected_id = Some(delivery_id.clone());
        view.loading_detail = true;
        view.detail = None;
        let Some(rpc) = self
            .state
            .read(cx)
            .request(method::GET_WEBHOOK_DELIVERY, get_delivery_payload(&task_id, &delivery_id), |reply| decode_webhook_delivery(&reply))
        else {
            view.loading_detail = false;
            view.detail = Some(Err("Not connected to server.".into()));
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = rpc.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                if let Some(view) = this.scheduled.deliveries.as_mut()
                    && view.seq == seq
                {
                    view.loading_detail = false;
                    view.detail = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn sync_filter_inputs(&mut self, draft: &Draft, window: &mut Window, cx: &mut Context<Self>) {
        let ids: HashSet<u64> = draft.filters.iter().map(|f| f.id).collect();
        self.scheduled.filter_inputs.retain(|id, _| ids.contains(id));
        for filter in &draft.filters {
            self.scheduled.filter_inputs.entry(filter.id).or_insert_with(|| {
                let path = cx.new(|cx| InputState::new(window, cx).default_value(filter.path.clone()).placeholder("body.action"));
                let value = cx.new(|cx| InputState::new(window, cx).default_value(filter.value.clone()).placeholder("opened"));
                (path, value)
            });
        }
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
        set(&inputs.thread_search.clone(), "", window, cx);
        set(&inputs.max_age.clone(), &draft.max_delivery_age_minutes, window, cx);
        set(&inputs.replay_key.clone(), &draft.replay_key, window, cx);
        set(&inputs.coalesce_key.clone(), &draft.coalesce_key, window, cx);
        set(&inputs.coalesce_window.clone(), &draft.coalesce_window_seconds, window, cx);
        set(&inputs.sig_header.clone(), &draft.signature_header, window, cx);
        set(&inputs.sig_prefix.clone(), &draft.signature_prefix, window, cx);
        set(&inputs.sig_secret.clone(), &draft.signature_secret, window, cx);
        let prompt = draft.prompt.clone();
        self.scheduled.inputs.prompt.clone().update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.scheduled.filter_inputs.clear();
        self.sync_filter_inputs(&draft, window, cx);
        self.scheduled.editor = Some(draft);
        self.scheduled.deliveries = None;
        self.scheduled.thread_picker_open = false;
        self.scheduled.editor_error = None;
        self.scheduled.save_error = None;
        self.scheduled.menu = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    fn new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_operate_tasks(cx) {
            return;
        }
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
        let title = value(&inputs.title);
        let time = value(&inputs.time_of_day);
        let run_at = value(&inputs.run_at);
        let interval = value(&inputs.interval);
        let base_ref = value(&inputs.base_ref);
        let checkout = value(&inputs.checkout);
        let max_age = value(&inputs.max_age);
        let replay_key = value(&inputs.replay_key);
        let coalesce_key = value(&inputs.coalesce_key);
        let coalesce_window = value(&inputs.coalesce_window);
        let sig_header = value(&inputs.sig_header);
        let sig_prefix = value(&inputs.sig_prefix);
        let sig_secret = value(&inputs.sig_secret);
        let prompt = inputs.prompt.read(cx).value().to_string();
        let filter_values: HashMap<u64, (String, String)> = self
            .scheduled
            .filter_inputs
            .iter()
            .map(|(id, (path_in, val_in))| (*id, (value(path_in), value(val_in))))
            .collect();
        if let Some(draft) = self.scheduled.editor.as_mut() {
            draft.title = title;
            draft.prompt = prompt;
            draft.time_of_day = time;
            draft.run_at = run_at;
            draft.interval_minutes = interval;
            draft.base_ref = base_ref;
            draft.existing_worktree_path = checkout;
            draft.max_delivery_age_minutes = max_age;
            draft.replay_key = replay_key;
            draft.coalesce_key = coalesce_key;
            draft.coalesce_window_seconds = coalesce_window;
            draft.signature_header = sig_header;
            draft.signature_prefix = sig_prefix;
            draft.signature_secret = sig_secret;
            for filter in &mut draft.filters {
                if let Some((path, val)) = filter_values.get(&filter.id) {
                    filter.path = path.clone();
                    filter.value = val.clone();
                }
            }
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
        if self.scheduled.saving || !self.can_operate_tasks(cx) {
            return;
        }
        self.read_editor_inputs(cx);
        let Some(config) = self.thread_settings(cx) else { return };
        let once = config.environment.capabilities.one_time_schedules == Some(true);
        let thread_modes = config.environment.capabilities.scheduled_task_thread_modes == Some(true);
        let ceiling = scheduled_tasks::max_runtime_mode(config.managed_policy.as_ref()).map(str::to_owned);
        let Some(draft) = self.scheduled.editor.clone() else { return };
        if draft.enabled && draft.schedule_mode == ScheduleMode::Webhook && self.webhook_denial(cx).is_some() {
            return;
        }
        let input = match draft.upsert_input_with_thread_modes(once, thread_modes, ceiling.as_deref(), field_to_iso) {
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

    pub(super) fn select_scheduled_task(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.scheduled.pending_task = Some(task_id.to_owned());
        self.apply_pending_scheduled_task(window, cx);
    }

    fn apply_pending_scheduled_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task_id) = self.scheduled.pending_task.as_deref() else { return };
        let Some(Ok(tasks)) = &self.scheduled.tasks else { return };
        let found = tasks.iter().find(|task| task.id == task_id).cloned();
        self.scheduled.pending_task = None;
        if let Some(task) = found {
            self.open_editor(Draft::from_task(&task, local_field), window, cx);
        }
    }

    pub(super) fn scheduled_tasks_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        self.ensure_task_stream(cx);
        self.apply_pending_scheduled_task(window, cx);
        let Some(config) = self.thread_settings(cx).cloned() else {
            return vec![Block::new(&[], self.task_empty("No environments available", "Connect an environment to manage scheduled tasks.", cx))];
        };
        if self.scheduled.deliveries.is_some() {
            return vec![Block::new(&[], self.deliveries_panel(cx))];
        }
        if self.scheduled.editor.is_some() {
            return vec![Block::new(&[], self.task_editor(&config, window, cx))];
        }
        let can_operate = self.can_operate_tasks(cx);
        let new_task = Button::new("new-scheduled-task", ButtonVariant::Outline, ButtonSize::Sm)
            .icon(Icon::Plus)
            .label("New task")
            .disabled(!can_operate)
            .on_click(cx.listener(|this, _, window, cx| this.new_task(window, cx)));
        let header = h_flex().justify_between().pr(r(8.)).child(section_title("Scheduled tasks")).child(new_task);
        let body = match &self.scheduled.tasks {
            None => self.task_empty("Loading scheduled tasks\u{2026}", "", cx),
            Some(Err(error)) => self.task_empty("Could not load scheduled tasks", &error.clone(), cx),
            Some(Ok(tasks)) if tasks.is_empty() => self.task_empty("No scheduled tasks", "No tasks match this environment and project selection.", cx),
            Some(Ok(tasks)) => {
                let tasks = tasks.clone();
                let now = now_ms();
                group(tasks.iter().map(|task| self.task_row(task, now, can_operate, cx)).collect(), cx).into_any_element()
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

    fn task_row(&self, task: &ScheduledTask, now: i64, can_operate: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let busy = self.scheduled.busy.contains(&task.id);
        let schedule_text = task.schedule.label_with_timezone(
            self.scheduled.time_zone.as_deref(),
            task.next_run_at.as_deref(),
            now,
            once_label,
        );
        let status = format!("{schedule_text} \u{00b7} {}", task.state_label(now));
        let badge = task.last_run_status.badge().map(|text| {
            let tone = match task.last_run_status {
                RunStatus::Failed => Tone::Error,
                RunStatus::Succeeded => Tone::Success,
                _ => Tone::Info,
            };
            tone_badge(text, tone, cx)
        });
        let thread_link = task
            .thread_id
            .as_ref()
            .filter(|_| task.effective_thread_mode() != ThreadMode::NewThread)
            .map(|thread_id| {
                let title = self.thread_title_for(thread_id, cx).unwrap_or_else(|| "Open thread".to_owned());
                let open_id = thread_id.clone();
                Button::new(SharedString::from(format!("scheduled-task-thread-{}", task.id)), ButtonVariant::GhostMuted, ButtonSize::Compact)
                    .label(title)
                    .on_click(cx.listener(move |this, _, window, cx| this.open_task_thread(open_id.clone(), window, cx)))
            });
        let (id, enabled, view) = (task.id.clone(), task.enabled, cx.entity().downgrade());
        let denial = (!task.enabled && task.is_webhook()).then(|| self.webhook_denial(cx)).flatten();
        let toggle = crate::components::managed::ManagedControl::new(
            SharedString::from(format!("managed-task-enabled-{}", task.id)),
            denial,
            Switch::new(SharedString::from(format!("scheduled-task-enabled-{}", task.id)), task.enabled)
                .disabled(busy || !can_operate)
                .on_change(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.task_rpc(&id, method::SET_ENABLED, scheduled_tasks::set_enabled_payload(&id, !enabled), cx));
                }),
        );
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
                            .children(thread_link)
                            .children(badge)
                            .children(task.last_run_error.clone().map(|error| div().text_color(hsla(p.destructive_foreground)).child(error))),
                    ),
            )
            .child(h_flex().flex_none().gap(r(8.)).child(self.task_menu(task, busy, can_operate, cx)).child(toggle))
            .into_any_element()
    }

    /// The row's "…" menu: Edit, Deliveries (webhook), Run now (non-webhook), Delete.
    fn task_menu(&self, task: &ScheduledTask, busy: bool, can_operate: bool, cx: &mut Context<Self>) -> AnyElement {
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
            let (edit_task, run_id, delete_id, delete_title) = (task.clone(), task.id.clone(), task.id.clone(), task.title.clone());
            let (deliv_id, deliv_title) = (task.id.clone(), task.title.clone());
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
                            .when(can_operate, |this| {
                                this.child(item("Edit", false).on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_editor(Draft::from_task(&edit_task, local_field), window, cx)
                                })))
                            })
                            .when(webhook, |this| {
                                this.child(item("Deliveries", false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_deliveries(deliv_id.clone(), deliv_title.clone(), cx)
                                })))
                            })
                            .when(!webhook && can_operate, |this| {
                                this.child(item("Run now", false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.task_rpc(&run_id, method::RUN_NOW, scheduled_tasks::id_payload(&run_id), cx)
                                })))
                            })
                            .when(can_operate, |this| {
                                this.child(div().my(r(4.)).h(px(1.)).bg(hsla(p.border))).child(item("Delete", true).on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirm_delete_task(delete_id.clone(), delete_title.clone(), cx)
                                })))
                            }),
                    ),
                )
                .with_priority(2),
            )
        });
        div().relative().flex_none().child(trigger).children(popup).into_any_element()
    }

    fn deliveries_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(view) = self.scheduled.deliveries.as_ref() else {
            return div().into_any_element();
        };
        let title = format!("Webhook deliveries · {}", view.task_title);
        let loading = view.loading;
        let header = h_flex()
            .justify_between()
            .gap(r(8.))
            .child(
                h_flex()
                    .gap(r(8.))
                    .child(
                        Button::new("deliveries-back", ButtonVariant::GhostMuted, ButtonSize::Sm)
                            .icon(Icon::ChevronLeft)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scheduled.deliveries = None;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title)),
            )
            .child(
                Button::new("deliveries-refresh", ButtonVariant::Outline, ButtonSize::Sm)
                    .icon(Icon::RefreshCw)
                    .label("Refresh")
                    .disabled(loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_deliveries(cx))),
            );

        let list_el: AnyElement = if view.loading && view.list.as_ref().is_ok_and(Vec::is_empty) {
            self.task_empty("Loading deliveries…", "", cx)
        } else {
            match &view.list {
                Err(err) => self.task_empty("Could not load deliveries", err, cx),
                Ok(items) if items.is_empty() => {
                    self.task_empty("No webhook deliveries yet", "Requests sent to this task's webhook URL appear here.", cx)
                }
                Ok(items) => {
                    let selected_id = view.selected_id.clone();
                    let rows: Vec<AnyElement> = items
                        .iter()
                        .map(|item| {
                            let is_selected = selected_id.as_deref() == Some(item.id.as_str());
                            let deliv_id = item.id.clone();
                            let label = webhook_delivery_label(&item.outcome, item.pending_window);
                            let tone = delivery_outcome_tone(&item.outcome);
                            let when = t3_core::sidebar::time::parse_iso(&item.received_at)
                                .map(once_label)
                                .unwrap_or_else(|| item.received_at.clone());
                            let meta = format!("{} · {} B · {}", item.method, item.body_bytes, when);
                            h_flex()
                                .id(SharedString::from(format!("webhook-delivery-{}", item.id)))
                                .px(r(14.))
                                .py(r(10.))
                                .gap(r(10.))
                                .cursor_pointer()
                                .when(is_selected, |row| row.bg(hsla(p.accent).opacity(0.6)))
                                .hover(|row| row.bg(hsla(p.accent).opacity(0.4)))
                                .child(tone_badge(label, tone, cx))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(r(2.))
                                        .child(div().text_xs().text_color(hsla(p.foreground)).child(meta))
                                        .children(item.error.clone().map(|err| div().text_xs().text_color(hsla(p.destructive_foreground)).child(err)))
                                        .when(!item.missing_fields.is_empty(), |col| {
                                            col.child(
                                                div()
                                                    .text_size(r(11.))
                                                    .text_color(hsla(p.warning_foreground))
                                                    .child(format!("Missing fields: {}", item.missing_fields.join(", "))),
                                            )
                                        }),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| this.select_delivery(deliv_id.clone(), cx)))
                                .into_any_element()
                        })
                        .collect();
                    group(rows, cx).into_any_element()
                }
            }
        };

        let detail_el: Option<AnyElement> = if view.loading_detail {
            Some(div().px(r(16.)).py(r(12.)).child(muted_text("Loading delivery details…", cx)).into_any_element())
        } else {
            view.detail.as_ref().map(|detail| match detail {
                Err(err) => div().px(r(16.)).py(r(12.)).text_xs().text_color(hsla(p.destructive_foreground)).child(err.clone()).into_any_element(),
                Ok(delivery) => {
                    let mono = cx.theme().mono_font_family.clone();
                    let mut box_el = v_flex()
                        .gap(r(10.))
                        .p(r(14.))
                        .rounded(r(10.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.card).opacity(0.5))
                        .child(
                            h_flex()
                                .gap(r(8.))
                                .child(tone_badge(
                                    webhook_delivery_label(&delivery.summary.outcome, delivery.summary.pending_window),
                                    delivery_outcome_tone(&delivery.summary.outcome),
                                    cx,
                                ))
                                .child(div().text_xs().text_color(hsla(p.muted_foreground)).child(format!(
                                    "{} · {} bytes · signature {}",
                                    delivery.summary.method,
                                    delivery.summary.body_bytes,
                                    if delivery.summary.signature_verified { "verified" } else { "not verified" }
                                ))),
                        );
                    if let Some(prompt) = &delivery.rendered_prompt {
                        box_el = box_el.child(
                            v_flex()
                                .gap(r(4.))
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Rendered prompt"))
                                .child(
                                    div()
                                        .p(r(8.))
                                        .rounded(r(6.))
                                        .bg(hsla(p.background))
                                        .font_family(mono.clone())
                                        .text_xs()
                                        .text_color(hsla(p.foreground))
                                        .child(prompt.clone()),
                                ),
                        );
                    }
                    if !delivery.query.is_empty() {
                        box_el = box_el.child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!("Query: {}", delivery.query)),
                        );
                    }
                    if !delivery.headers.is_empty() {
                        let headers_text = delivery
                            .headers
                            .iter()
                            .map(|(k, v)| format!("{k}: {v}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        box_el = box_el.child(
                            v_flex()
                                .gap(r(4.))
                                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Headers"))
                                .child(
                                    div()
                                        .p(r(8.))
                                        .rounded(r(6.))
                                        .bg(hsla(p.background))
                                        .font_family(mono.clone())
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(headers_text),
                                ),
                        );
                    }
                    box_el = box_el.child(
                        v_flex()
                            .gap(r(4.))
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(
                                if delivery.body_truncated { "Body (truncated)" } else { "Body" },
                            ))
                            .child(
                                div()
                                    .p(r(8.))
                                    .rounded(r(6.))
                                    .bg(hsla(p.background))
                                    .font_family(mono)
                                    .text_xs()
                                    .text_color(hsla(p.foreground))
                                    .child(if delivery.body.is_empty() { "(empty)".to_owned() } else { delivery.body.clone() }),
                            ),
                    );
                    box_el.into_any_element()
                }
            })
        };

        v_flex().gap(r(12.)).child(header).child(list_el).children(detail_el).into_any_element()
    }

    fn task_editor(&mut self, config: &ServerConfig, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(draft) = self.scheduled.editor.clone() else { return div().into_any_element() };
        self.sync_filter_inputs(&draft, window, cx);
        let projects = self.shell_projects(cx);
        let inputs = &self.scheduled.inputs;
        let field = |input: &Entity<InputState>, width: f32| div().w(r(width)).child(Input::new(input).small());
        let once_supported = config.environment.capabilities.one_time_schedules == Some(true);
        let thread_modes_supported = config.environment.capabilities.scheduled_task_thread_modes == Some(true);
        let editing = draft.editing_id.is_some();

        let project_state = cx.entity().downgrade();
        let project_select = Select::new("scheduled-task-project", project_options(&projects, &draft.project_id))
            .selected(draft.project_id.clone())
            .label(projects.iter().find(|project| project.id == draft.project_id).map_or_else(|| "Select a project".to_owned(), |project| project.title.clone()))
            .width(240.)
            .on_change(move |value, _, cx| {
                let _ = project_state.update(cx, |this, cx| this.edit_draft(|draft| draft.change_project(value.to_string()), cx));
            });

        // Destination: thread mode selector + optional destination thread picker.
        let mode_state = cx.entity().downgrade();
        let mode_select = thread_modes_supported.then(|| {
            Select::new(
                "scheduled-task-thread-mode",
                ThreadMode::ALL.iter().map(|mode| SelectOption::new(mode.as_str(), mode.label())).collect(),
            )
            .selected(draft.thread_mode.as_str())
            .width(260.)
            .on_change(move |value, _, cx| {
                if let Some(mode) = ThreadMode::parse(&value) {
                    let _ = mode_state.update(cx, |this, cx| {
                        this.scheduled.thread_picker_open = false;
                        this.edit_draft(|draft| draft.set_thread_mode(mode), cx);
                    });
                }
            })
        });

        let show_thread_picker_row = draft.thread_mode != ThreadMode::NewThread || draft.thread_id.is_some();
        let destination_below: Option<AnyElement> = show_thread_picker_row.then(|| {
            let current_label = draft
                .thread_id
                .as_deref()
                .map(|id| self.thread_title_for(id, cx).unwrap_or_else(|| format!("Thread {id}")))
                .unwrap_or_else(|| "Started by the first run".to_owned());
            let mut panel = v_flex().gap(r(8.)).mt(r(8.)).pb(r(10.));
            let mut bar = h_flex().gap(r(8.)).child(
                Button::new("scheduled-task-pick-thread", ButtonVariant::Outline, ButtonSize::Sm)
                    .label(current_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scheduled.thread_picker_open = !this.scheduled.thread_picker_open;
                        cx.notify();
                    })),
            );
            if draft.thread_id.is_some() {
                bar = bar.child(
                    Button::new("scheduled-task-unbind-thread", ButtonVariant::GhostMuted, ButtonSize::Sm)
                        .label("Clear thread")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.scheduled.thread_picker_open = false;
                            this.edit_draft(Draft::unbind_from_thread, cx);
                        })),
                );
            }
            panel = panel.child(bar);
            if draft.thread_id.is_some() {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child("Deleting that thread stops this task."),
                );
            }
            if self.scheduled.thread_picker_open {
                let query = inputs.thread_search.read(cx).value().to_string();
                let threads = self.state.read(cx).shell.snapshot.as_ref().map(|s| s.threads.clone()).unwrap_or_default();
                let candidates = destination_threads(
                    threads.iter().map(|t| {
                        (
                            t.id.as_str(),
                            t.project_id.as_str(),
                            t.title.as_str(),
                            t.updated_at.as_str(),
                            t.archived_at.is_some(),
                            &t.lineage,
                        )
                    }),
                    &draft.project_id,
                    draft.thread_id.as_deref(),
                    &query,
                );
                let mut list = v_flex().gap(r(2.)).max_h(r(180.)).id("scheduled-task-thread-list").overflow_y_scroll();
                if candidates.is_empty() {
                    list = list.child(div().px(r(8.)).py(r(6.)).text_xs().text_color(hsla(p.muted_foreground)).child("No matching threads in this project."));
                } else {
                    for candidate in candidates.into_iter().take(40) {
                        let cid = candidate.id.clone();
                        let pid = candidate.project_id.clone();
                        list = list.child(
                            h_flex()
                                .id(SharedString::from(format!("scheduled-dest-thread-{}", candidate.id)))
                                .px(r(8.))
                                .py(r(5.))
                                .rounded(r(6.))
                                .cursor_pointer()
                                .hover(|row| row.bg(hsla(p.accent)))
                                .text_xs()
                                .text_color(hsla(p.foreground))
                                .child(candidate.title)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.scheduled.thread_picker_open = false;
                                    this.edit_draft(|draft| draft.move_to_thread(cid.clone(), pid.clone()), cx);
                                })),
                        );
                    }
                }
                panel = panel.child(
                    v_flex()
                        .gap(r(6.))
                        .p(r(8.))
                        .rounded(r(8.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.7))
                        .bg(hsla(p.card).opacity(0.5))
                        .child(Input::new(&inputs.thread_search).small())
                        .child(list),
                );
            }
            panel.into_any_element()
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
                        h_flex().gap(r(8.)).child(field(&inputs.base_ref, 160.)).child(
                            Switch::new("scheduled-task-start-from-origin", draft.start_from_origin).on_change(move |on, _, cx| {
                                let _ = origin_state.update(cx, |this, cx| this.edit_draft(|draft| draft.start_from_origin = on, cx));
                            }),
                        ),
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

        let modes = h_flex().gap(r(4.)).children(
            ScheduleMode::ALL
                .into_iter()
                .filter(|mode| *mode != ScheduleMode::Once || once_supported || draft.schedule_mode == ScheduleMode::Once)
                .map(|mode| {
                    let selected = draft.schedule_mode == mode;
                    let denial = (mode == ScheduleMode::Webhook && !selected).then(|| self.webhook_denial(cx)).flatten();
                    crate::components::managed::ManagedControl::new(
                        SharedString::from(format!("managed-task-mode-{}", mode.label())),
                        denial,
                        Button::new(
                            SharedString::from(format!("scheduled-task-mode-{}", mode.label())),
                            if selected { ButtonVariant::Default } else { ButtonVariant::Outline },
                            ButtonSize::Compact,
                        )
                        .label(mode.label())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if mode == ScheduleMode::Webhook && this.webhook_denial(cx).is_some() {
                                return;
                            }
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
                        })),
                    )
                }),
        );
        let tz_label = schedule_time_zone_label(self.scheduled.time_zone.as_deref(), None, now_ms());
        let schedule_detail: AnyElement = match draft.schedule_mode {
            ScheduleMode::Fixed => h_flex()
                .flex_wrap()
                .gap(r(8.))
                .text_sm()
                .child("Run at")
                .child(field(&inputs.time_of_day, 80.))
                .children(tz_label.map(|tz| div().text_xs().text_color(hsla(p.muted_foreground)).child(tz)))
                .child("on")
                .children(DAY_TOGGLES.into_iter().map(|(day, label)| {
                    let on = draft.weekdays.contains(&day);
                    Button::new(
                        SharedString::from(format!("scheduled-task-day-{day}")),
                        if on { ButtonVariant::Default } else { ButtonVariant::Outline },
                        ButtonSize::Compact,
                    )
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.edit_draft(|draft| draft.toggle_weekday(day), cx)))
                }))
                .into_any_element(),
            ScheduleMode::Once => h_flex().gap(r(8.)).text_sm().child("Run on").child(field(&inputs.run_at, 180.)).into_any_element(),
            ScheduleMode::Interval => h_flex().gap(r(8.)).text_sm().child("Run every").child(field(&inputs.interval, 72.)).child("minutes").into_any_element(),
            ScheduleMode::Webhook => self.webhook_editor_detail(&draft, cx),
        };

        let enabled_state = cx.entity().downgrade();
        let mut destination_row = Row::new("scheduled-task-destination", "Destination").description(draft.thread_mode.detail(draft.thread_id.is_some()));
        if let Some(select) = mode_select {
            destination_row = destination_row.control(select);
        }
        if let Some(below) = destination_below {
            destination_row = destination_row.below(below);
        }
        let rows = vec![
            Row::new("scheduled-task-name", "Name").control(field(&inputs.title, 280.)).into_any_element(),
            destination_row.into_any_element(),
            Row::new("scheduled-task-project-row", "Project").control(project_select).into_any_element(),
            Row::new("scheduled-task-workspace-row", "Workspace").control(workspace).into_any_element(),
        ]
        .into_iter()
        .chain(workspace_detail)
        .chain([
            Row::new("scheduled-task-prompt", "Prompt").below(div().mt(r(12.)).pb(r(14.)).child(Textarea::new(&inputs.prompt))).into_any_element(),
            Row::new("scheduled-task-model-row", "Model").control(model).into_any_element(),
            Row::new("scheduled-task-schedule", "Schedule")
                .control(modes)
                .below(
                    div()
                        .mt(r(12.))
                        .pb(r(14.))
                        .children(self.webhook_denial(cx).map(|reason| {
                            crate::components::managed::notice(
                                if draft.schedule_mode == ScheduleMode::Webhook {
                                    format!("{reason} Switch this task to a time or an interval, or turn it off, to save it.")
                                } else {
                                    reason
                                },
                                cx,
                            )
                        }))
                        .child(schedule_detail),
                )
                .into_any_element(),
            Row::new("scheduled-task-enabled", "Enabled")
                .description("Disabled tasks stay saved but do not run.")
                .control(Switch::new("scheduled-task-enabled-switch", draft.enabled).on_change(move |on, _, cx| {
                    let _ = enabled_state.update(cx, |this, cx| this.edit_draft(|draft| draft.enabled = on, cx));
                }))
                .into_any_element(),
        ])
        .collect();

        let error = self
            .scheduled
            .editor_error
            .clone()
            .map(|invalid| (invalid.title, invalid.description))
            .or_else(|| self.scheduled.save_error.clone().map(|message| ("Could not save scheduled task".to_owned(), message)));
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
                            .disabled(
                                self.scheduled.saving
                                    || !connected
                                    || (draft.enabled && draft.schedule_mode == ScheduleMode::Webhook && self.webhook_denial(cx).is_some()),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.save_task(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn webhook_editor_detail(&self, draft: &Draft, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let inputs = &self.scheduled.inputs;
        let task = draft
            .editing_id
            .as_ref()
            .and_then(|id| self.scheduled.tasks.as_ref()?.as_ref().ok()?.iter().find(|task| &task.id == id).cloned());
        let rotate_task_id = draft.editing_id.clone();
        let address = task.and_then(|task| task.webhook).map(|hook| webhook_address(&hook, self.state.read(cx).environment()));
        let rotating = self.scheduled.rotating_url;

        let address_el: AnyElement = match address {
            Some((url, copyable, note)) => v_flex()
                .gap(r(4.))
                .child(
                    h_flex()
                        .gap(r(8.))
                        .child(div().flex_1().min_w_0().truncate().font_family(cx.theme().mono_font_family.clone()).text_color(hsla(p.foreground)).child(url.clone()))
                        .when(copyable, |this| {
                            this.child(
                                Button::new("scheduled-task-copy-webhook", ButtonVariant::Outline, ButtonSize::Compact)
                                    .icon(Icon::Copy)
                                    .label("Copy")
                                    .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()))),
                            )
                        })
                        .when_some(rotate_task_id, |this, task_id| {
                            this.child(
                                Button::new("scheduled-task-rotate-webhook", ButtonVariant::Outline, ButtonSize::Compact)
                                    .label(if rotating { "Rotating…" } else { "Rotate URL" })
                                    .disabled(rotating)
                                    .on_click(cx.listener(move |this, _, _, cx| this.confirm_rotate_webhook_url(task_id.clone(), cx))),
                            )
                        }),
                )
                .when(!note.is_empty(), |this| this.child(note))
                .into_any_element(),
            None => div().child("The URL appears after you save.").into_any_element(),
        };

        // Filters CRUD.
        let mut filters_box = v_flex().gap(r(6.)).child(
            h_flex()
                .justify_between()
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Filters (all must match)"))
                .child(
                    Button::new("scheduled-task-add-filter", ButtonVariant::Outline, ButtonSize::Compact)
                        .icon(Icon::Plus)
                        .label("Add filter")
                        .disabled(draft.filters.len() >= MAX_WEBHOOK_FILTERS)
                        .on_click(cx.listener(|this, _, _, cx| this.edit_draft(Draft::add_filter, cx))),
                ),
        );
        for filter in &draft.filters {
            let Some((path_in, val_in)) = self.scheduled.filter_inputs.get(&filter.id) else { continue };
            let fid_op = filter.id;
            let fid_rm = filter.id;
            let view_op = cx.entity().downgrade();
            filters_box = filters_box.child(
                h_flex()
                    .gap(r(6.))
                    .on_key_down(stop_slash_shortcut)
                    .child(div().w(r(170.)).child(Input::new(path_in).small()))
                    .child(
                        Select::new(
                            SharedString::from(format!("scheduled-filter-op-{}", filter.id)),
                            WebhookFilterOp::ALL.iter().map(|op| SelectOption::new(op.as_str(), op.label())).collect(),
                        )
                        .selected(filter.op.as_str())
                        .width(140.)
                        .on_change(move |value, _, cx| {
                            if let Some(op) = WebhookFilterOp::parse(&value) {
                                let _ = view_op.update(cx, |this, cx| {
                                    this.edit_draft(
                                        |draft| {
                                            if let Some(row) = draft.filters.iter_mut().find(|f| f.id == fid_op) {
                                                row.op = op;
                                            }
                                        },
                                        cx,
                                    );
                                });
                            }
                        }),
                    )
                    .when(filter.op != WebhookFilterOp::Exists, |row| row.child(div().flex_1().min_w_0().child(Input::new(val_in).small())))
                    .child(
                        Button::new(SharedString::from(format!("scheduled-filter-rm-{}", filter.id)), ButtonVariant::GhostMuted, ButtonSize::IconMicro)
                            .icon(Icon::X)
                            .tooltip("Remove filter")
                            .on_click(cx.listener(move |this, _, _, cx| this.edit_draft(|draft| draft.remove_filter(fid_rm), cx))),
                    ),
            );
        }

        // Signature section.
        let sig_switch_view = cx.entity().downgrade();
        let sig_enc_view = cx.entity().downgrade();
        let mut sig_box = v_flex().gap(r(8.)).child(
            h_flex()
                .justify_between()
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Require HMAC-SHA256 signature"))
                .child(Switch::new("scheduled-task-sig-enabled", draft.signature_enabled).on_change(move |on, _, cx| {
                    let _ = sig_switch_view.update(cx, |this, cx| this.edit_draft(|draft| draft.signature_enabled = on, cx));
                })),
        );
        if draft.signature_enabled {
            let secret_hint = if draft.has_saved_signature_secret {
                "Secret is saved on the server. Leave blank to keep it, or enter a new secret to replace it."
            } else {
                "Shared HMAC-SHA256 signing secret."
            };
            sig_box = sig_box
                .child(
                    h_flex()
                        .gap(r(8.))
                        .child(
                            v_flex()
                                .flex_1()
                                .gap(r(4.))
                                .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child("Header"))
                                .child(Input::new(&inputs.sig_header).small()),
                        )
                        .child(
                            v_flex()
                                .w(r(120.))
                                .gap(r(4.))
                                .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child("Encoding"))
                                .child(
                                    Select::new(
                                        "scheduled-task-sig-encoding",
                                        SignatureEncoding::ALL.iter().map(|enc| SelectOption::new(enc.as_str(), enc.label())).collect(),
                                    )
                                    .selected(draft.signature_encoding.as_str())
                                    .width(120.)
                                    .on_change(move |value, _, cx| {
                                        if let Some(enc) = SignatureEncoding::parse(&value) {
                                            let _ = sig_enc_view.update(cx, |this, cx| this.edit_draft(|draft| draft.signature_encoding = enc, cx));
                                        }
                                    }),
                                ),
                        )
                        .child(
                            v_flex()
                                .w(r(120.))
                                .gap(r(4.))
                                .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child("Prefix"))
                                .child(Input::new(&inputs.sig_prefix).small()),
                        ),
                )
                .child(
                    v_flex()
                        .gap(r(4.))
                        .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child("Signing secret"))
                        .child(Input::new(&inputs.sig_secret).small())
                        .child(div().text_size(r(11.)).text_color(hsla(p.muted_foreground)).child(secret_hint)),
                );
        }

        v_flex()
            .gap(r(12.))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .child(address_el)
            .child("Each request runs the prompt. Use {{body.path}}, {{headers.name}}, {{query.name}}, {{body}} or {{request}} in the prompt; only what it names reaches the agent.")
            .child(
                v_flex()
                    .gap(r(4.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Max delivery age (minutes)"))
                    .child(div().w(r(180.)).child(Input::new(&inputs.max_age).small())),
            )
            .child(filters_box)
            .child(
                v_flex()
                    .gap(r(6.))
                    .child(div().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Deduplication"))
                    .child(
                        h_flex()
                            .gap(r(8.))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap(r(4.))
                                    .child(div().text_size(r(11.)).child("Duplicate request key"))
                                    .child(Input::new(&inputs.replay_key).small()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap(r(4.))
                                    .child(div().text_size(r(11.)).child("Coalesce key"))
                                    .child(Input::new(&inputs.coalesce_key).small()),
                            )
                            .child(
                                v_flex()
                                    .w(r(130.))
                                    .gap(r(4.))
                                    .child(div().text_size(r(11.)).child("Window (sec)"))
                                    .child(Input::new(&inputs.coalesce_window).small()),
                            ),
                    ),
            )
            .child(sig_box)
            .into_any_element()
    }
}
