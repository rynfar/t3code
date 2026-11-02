//! Composer: the prompt, its footer controls, and the panels for what the agent waits on. Ports
//! `ChatComposer.tsx` (layout, placeholders), `useComposerSend.ts` (send, launch, the approval
//! redirect), `usePendingThreadRequests.ts` (answering), `useComposerSelection.ts` (per-thread
//! model, permission and mode choices) and `composerCommandKey.ts` (keys). Choices persist the
//! way the web persists them: permissions and mode with their `thread.*.set` commands before the
//! next turn, the model on the turn itself.

pub(crate) mod controls;
mod model_picker;
mod pending;
mod view;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, FocusHandle, KeyBinding, Subscription, Task, Window, actions, prelude::*};
use gpui_component::input::{InputEvent, OutdentInline, TextareaState};
use serde_json::Value;
use t3_core::commands::{self, ApprovalDecision, ComposerMessage, LaunchThread, method};
use t3_core::composer::{self as rules, FollowUpBehavior, SubmitIntent};
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::requests::{self, DraftAnswer, PendingRequests};
use t3_core::server_config::{InteractionMode, OptionDescriptor, RuntimeMode};
use t3_core::traits::{self, OptionSelection};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::icons::Icon;
use model_picker::{ModelPicker, PickerEvent, PickerInput};

actions!(composer, [TogglePlanMode, ToggleModelPicker, OpenPermissions]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("shift-tab", TogglePlanMode, Some("Composer > Input")),
        KeyBinding::new("cmd-shift-m", ToggleModelPicker, None),
        KeyBinding::new("cmd-shift-e", ToggleModelPicker, None),
        KeyBinding::new("cmd-shift-a", OpenPermissions, None),
    ]);
    model_picker::init(cx);
}

/// `runtimeModeConfig` icons.
pub(crate) fn runtime_mode_icon(mode: RuntimeMode) -> Icon {
    match mode {
        RuntimeMode::ApprovalRequired => Icon::Lock,
        RuntimeMode::AutoAcceptEdits => Icon::PenLine,
        RuntimeMode::Auto => Icon::Sparkles,
        RuntimeMode::FullAccess => Icon::ShieldOff,
    }
}

/// What the composer writes to: an existing thread or the new-thread draft of a project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    Thread(String),
    Draft(String),
}

/// The user's choices for one target, before they are sent.
#[derive(Debug, Clone, Default)]
struct Choices {
    model: Option<ModelSelection>,
    runtime_mode: Option<RuntimeMode>,
    interaction_mode: Option<InteractionMode>,
}

/// What the composer reads from app state, kept so streamed events that change nothing it shows
/// do not repaint it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Snapshot {
    target: Option<Target>,
    project_id: Option<String>,
    running: bool,
    thread_model: Option<ModelSelection>,
    thread_runtime: Option<RuntimeMode>,
    thread_interaction: Option<InteractionMode>,
    /// The thread has run, so switching providers needs a handoff it may not support.
    started: bool,
    handoff: bool,
    /// The thread's projection has arrived, so `pending` is authoritative.
    loaded: bool,
    pending: PendingRequests,
}

/// Everything the footer and send read, resolved against the server config.
#[derive(Debug, Clone, Default)]
struct Resolved {
    model: Option<ModelSelection>,
    model_label: String,
    driver: String,
    descriptors: Vec<OptionDescriptor>,
    runtime_modes: Vec<RuntimeMode>,
    runtime_mode: RuntimeMode,
    default_runtime_mode: RuntimeMode,
    plan_enabled: bool,
    interaction_mode: InteractionMode,
    locked_instance: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Answers {
    thread_id: String,
    drafts: HashMap<String, DraftAnswer>,
    index: usize,
}

pub struct Composer {
    state: Entity<AppState>,
    input: Entity<TextareaState>,
    picker: Entity<ModelPicker>,
    picker_open: bool,
    permissions_open: bool,
    approval_menu_open: bool,
    choices: HashMap<Target, Choices>,
    /// The model and mode a new draft starts from: what the thread before it ran.
    carry: (Option<ModelSelection>, Option<InteractionMode>),
    snapshot: Snapshot,
    responding: HashSet<String>,
    answers: HashMap<String, Answers>,
    /// The request whose active question the prompt edits, and the prompt it set aside.
    answering: Option<String>,
    stash: Option<String>,
    sending: bool,
    has_text: bool,
    error: Option<String>,
    auto_advance: Option<Task<()>>,
    picker_dismissed: Option<Instant>,
    menu_dismissed: Option<Instant>,
    input_state: Option<(&'static str, bool)>,
    /// The question banner takes focus for its digit shortcuts while the prompt cannot type.
    question_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 9)
                .placeholder(rules_placeholder())
                .submit_on_enter(true)
        });
        let picker = cx.new(|cx| ModelPicker::new(state.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { secondary, shift: false } => this.on_enter(*secondary, window, cx),
                InputEvent::Change => this.on_prompt_change(cx),
                _ => {}
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig) {
                    this.refresh(*event == AppEvent::ServerConfig, window, cx);
                }
            }),
            cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| this.on_picker(event, window, cx)),
        ];
        let mut composer = Composer {
            state,
            input,
            picker,
            picker_open: false,
            permissions_open: false,
            approval_menu_open: false,
            choices: HashMap::new(),
            carry: (None, None),
            snapshot: Snapshot::default(),
            responding: HashSet::new(),
            answers: HashMap::new(),
            answering: None,
            stash: None,
            sending: false,
            has_text: false,
            error: None,
            auto_advance: None,
            picker_dismissed: None,
            menu_dismissed: None,
            input_state: None,
            question_focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        composer.refresh(true, window, cx);
        composer
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn read_snapshot(&self, cx: &App) -> Snapshot {
        let state = self.state.read(cx);
        let target = match (&state.selected_thread, &state.draft_project) {
            (Some(id), _) => Some(Target::Thread(id.clone())),
            (None, Some(project)) => Some(Target::Draft(project.clone())),
            _ => None,
        };
        let Some(Target::Thread(thread_id)) = &target else {
            let project_id = state.draft_project.clone();
            return Snapshot { target, project_id, ..Snapshot::default() };
        };
        let shell = state.shell.thread(thread_id);
        let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == *thread_id);
        Snapshot {
            project_id: shell.map(|thread| thread.project_id.clone()),
            running: state.is_selected_thread_running(),
            thread_model: shell.map(|thread| thread.model_selection.clone()),
            thread_runtime: shell.and_then(|thread| RuntimeMode::parse(&thread.runtime_mode)),
            thread_interaction: shell.and_then(|thread| InteractionMode::parse(&thread.interaction_mode)),
            started: projection.is_some_and(|projection| !projection.runs.is_empty()),
            handoff: projection.is_some_and(|projection| {
                projection.provider_sessions.iter().any(|session| {
                    session.value.pointer("/capabilities/sessions/supportsProviderSwitchingViaHandoff") == Some(&Value::Bool(true))
                })
            }),
            loaded: projection.is_some(),
            pending: projection.map(requests::pending_requests).unwrap_or_default(),
            target,
        }
    }

    /// Re-reads app state. Notifies only when something the composer shows changed.
    fn refresh(&mut self, config_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.read_snapshot(cx);
        if next == self.snapshot && !config_changed {
            return;
        }
        if next.target != self.snapshot.target {
            if matches!(next.target, Some(Target::Draft(_))) && matches!(self.snapshot.target, Some(Target::Thread(_))) {
                let resolved = self.resolve(cx);
                self.carry = (resolved.model, Some(resolved.interaction_mode));
            }
            self.picker_open = false;
            self.permissions_open = false;
            self.approval_menu_open = false;
            self.error = None;
            if matches!(next.target, Some(Target::Draft(_))) {
                self.focus(window, cx);
            }
        }
        // `pruneResolvedUserInputDrafts`: only this thread's resolved drafts go; others wait.
        let pending_ids: Vec<&String> = next.pending.user_inputs.iter().map(|input| &input.request_id).collect();
        if let (Some(Target::Thread(thread_id)), true) = (&next.target, next.loaded) {
            self.answers.retain(|request_id, answers| answers.thread_id != *thread_id || pending_ids.contains(&request_id));
        }
        self.responding.retain(|request_id| {
            pending_ids.contains(&request_id) || next.pending.approvals.iter().any(|approval| approval.request_id == *request_id)
        });
        self.snapshot = next;
        self.sync_answering(window, cx);
        if self.picker_open {
            let input = self.picker_input(cx);
            self.picker.update(cx, |picker, cx| picker.update_input(input, cx));
        }
        cx.notify();
    }

    fn target_project(&self) -> Option<&str> {
        self.snapshot.project_id.as_deref()
    }

    fn choices(&self) -> Option<&Choices> {
        self.snapshot.target.as_ref().and_then(|target| self.choices.get(target))
    }

    fn choices_mut(&mut self) -> Option<&mut Choices> {
        let target = self.snapshot.target.clone()?;
        Some(self.choices.entry(target).or_default())
    }

    fn resolve(&self, cx: &App) -> Resolved {
        let state = self.state.read(cx);
        let config = state.server_config.config.as_ref();
        let choices = self.choices().cloned().unwrap_or_default();
        let draft = match &self.snapshot.target {
            Some(Target::Draft(project)) => {
                Some(rules::draft_defaults(config, project, self.carry.0.as_ref(), self.carry.1))
            }
            _ => None,
        };
        let chosen = choices
            .model
            .or_else(|| self.snapshot.thread_model.clone())
            .or_else(|| draft.as_ref().and_then(|draft| draft.model_selection.clone()));
        let entries = config.map(|config| providers::entries(config, self.target_project())).unwrap_or_default();
        let model = match config {
            Some(_) => rules::effective_selection(&entries, chosen.as_ref()),
            None => chosen,
        };
        let provider = model.as_ref().and_then(|model| config?.provider(&model.instance_id));
        let provider_model = model.as_ref().and_then(|selection| provider?.models.iter().find(|m| m.slug == selection.model));
        let descriptors = provider_model
            .and_then(|model| model.capabilities.as_ref())
            .map(|caps| {
                let explicit = traits::selections(model.as_ref().and_then(|m| m.options.as_ref()));
                traits::resolve_descriptors(&caps.option_descriptors, &explicit)
            })
            .unwrap_or_default();
        let runtime = choices
            .runtime_mode
            .or(self.snapshot.thread_runtime)
            .or_else(|| draft.as_ref().map(|draft| draft.runtime_mode))
            .unwrap_or_default();
        let (runtime_modes, runtime_mode) = rules::compatible_runtime_modes(runtime, provider);
        let interaction = choices
            .interaction_mode
            .or(self.snapshot.thread_interaction)
            .or_else(|| draft.as_ref().map(|draft| draft.interaction_mode))
            .unwrap_or_default();
        let (plan_enabled, interaction_mode) = rules::interaction_mode(state.client_settings.plan_mode_enabled, provider, interaction);
        let default_runtime_mode = config
            .map(|config| {
                self.target_project()
                    .and_then(|id| config.settings.project_settings_overrides.get(id))
                    .and_then(|overrides| overrides.default_runtime_mode)
                    .unwrap_or(config.settings.default_runtime_mode)
            })
            .unwrap_or_default();
        let locked_instance = (self.snapshot.started && !self.snapshot.handoff)
            .then(|| self.snapshot.thread_model.as_ref().map(|model| model.instance_id.clone()))
            .flatten();
        Resolved {
            model_label: provider_model.map(|m| providers::trigger_model_name(m).to_owned()).or_else(|| model.as_ref().map(|m| m.model.clone())).unwrap_or_default(),
            driver: provider.map(|p| p.driver.clone()).unwrap_or_default(),
            model,
            descriptors,
            runtime_modes,
            runtime_mode,
            default_runtime_mode,
            plan_enabled,
            interaction_mode,
            locked_instance,
        }
    }

    fn picker_input(&self, cx: &App) -> PickerInput {
        let resolved = self.resolve(cx);
        PickerInput {
            project_id: self.snapshot.project_id.clone(),
            selection: resolved.model,
            driver: resolved.driver,
            descriptors: resolved.descriptors,
            runtime_mode: resolved.runtime_mode,
            runtime_modes: resolved.runtime_modes,
            locked_instance: resolved.locked_instance,
        }
    }

    pub fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.target.is_none() {
            return;
        }
        self.picker_open = !self.picker_open;
        self.permissions_open = false;
        if self.picker_open {
            let input = self.picker_input(cx);
            self.picker.update(cx, |picker, cx| picker.open(input, window, cx));
        } else {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// `composer.mode`: the permissions chip when it shows, else the model picker that holds them.
    pub fn open_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.target.is_none() {
            return;
        }
        let resolved = self.resolve(cx);
        if resolved.runtime_mode != resolved.default_runtime_mode {
            self.picker_open = false;
            self.permissions_open = !self.permissions_open;
            cx.notify();
        } else {
            self.toggle_picker(window, cx);
        }
    }

    fn on_picker(&mut self, event: &PickerEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PickerEvent::Model { instance_id, model } => {
                let selection = ModelSelection { instance_id: instance_id.clone(), model: model.clone(), options: None };
                if let Some(choices) = self.choices_mut() {
                    choices.model = Some(selection);
                }
            }
            PickerEvent::Options(selections) => self.set_options(selections, cx),
            PickerEvent::RuntimeMode(mode) => self.set_runtime_mode(*mode, cx),
            PickerEvent::Dismiss => {
                self.picker_open = false;
                self.focus(window, cx);
            }
        }
        if self.picker_open {
            let input = self.picker_input(cx);
            self.picker.update(cx, |picker, cx| picker.update_input(input, cx));
        }
        cx.notify();
    }

    fn set_options(&mut self, selections: &[OptionSelection], cx: &App) {
        let Some(model) = self.resolve(cx).model else { return };
        if let Some(choices) = self.choices_mut() {
            choices.model = Some(traits::model_selection(&model.instance_id, &model.model, selections));
        }
    }

    fn set_runtime_mode(&mut self, mode: RuntimeMode, cx: &mut Context<Self>) {
        if let Some(choices) = self.choices_mut() {
            choices.runtime_mode = Some(mode);
        }
        self.permissions_open = false;
        cx.notify();
    }

    fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        if !resolved.plan_enabled {
            window.dispatch_action(Box::new(OutdentInline), cx);
            return;
        }
        let next = match resolved.interaction_mode {
            InteractionMode::Plan => InteractionMode::Default,
            InteractionMode::Default => InteractionMode::Plan,
        };
        if let Some(choices) = self.choices_mut() {
            choices.interaction_mode = Some(next);
        }
        cx.notify();
    }

    fn prompt(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn set_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.has_text = !text.trim().is_empty();
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
    }

    fn on_enter(&mut self, modifier: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt(cx);
        let settings = self.state.read(cx).client_settings;
        let draft = matches!(self.snapshot.target, Some(Target::Draft(_)));
        match rules::submit_intent(modifier, self.snapshot.running, draft, settings.send_shortcut, &prompt) {
            Some(intent) => self.submit(intent, window, cx),
            None => self.input.update(cx, |input, cx| input.insert("\n", window, cx)),
        }
    }

    /// The textarea draws itself; the composer repaints only when what it shows depends on the
    /// text: the send button's enabled state, or the active answer.
    fn on_prompt_change(&mut self, cx: &mut Context<Self>) {
        let text = self.prompt(cx);
        let has_text = !text.trim().is_empty();
        let answering = self.active_question();
        if let Some((request_id, question)) = &answering {
            let thread_id = self.thread_id().unwrap_or_default();
            let answers = self.answers.entry(request_id.clone()).or_insert_with(|| Answers { thread_id, ..Answers::default() });
            let next = requests::set_custom_answer(question, answers.drafts.get(&question.id), &text);
            answers.drafts.insert(question.id.clone(), next);
        }
        if answering.is_some() || has_text != self.has_text {
            self.has_text = has_text;
            cx.notify();
        }
    }

    fn thread_id(&self) -> Option<String> {
        match &self.snapshot.target {
            Some(Target::Thread(id)) => Some(id.clone()),
            _ => None,
        }
    }

    /// Submit, as the send button and Enter do.
    fn submit(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        if !self.snapshot.pending.user_inputs.is_empty() {
            self.advance(window, cx);
            return;
        }
        let text = self.prompt(cx).trim().to_owned();
        if text.is_empty() {
            return;
        }
        match self.snapshot.target.clone() {
            Some(Target::Draft(project_id)) => self.launch(project_id, text, intent, window, cx),
            Some(Target::Thread(thread_id)) => self.send(thread_id, text, intent, window, cx),
            None => {}
        }
    }

    /// The model selection a turn carries: only the options the user chose.
    fn dispatch_selection(resolved: &Resolved) -> Option<ModelSelection> {
        let model = resolved.model.as_ref()?;
        let explicit = traits::selections(model.options.as_ref());
        Some(traits::model_selection(
            &model.instance_id,
            &model.model,
            &traits::explicit_selections(&resolved.descriptors, &explicit),
        ))
    }

    fn launch(&mut self, project_id: String, text: String, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        let Some(selection) = Self::dispatch_selection(&resolved) else {
            self.error = Some("Enable a provider in Settings to send a message".into());
            cx.notify();
            return;
        };
        let (thread_id, command_id, message_id) = (uuid_v4(), uuid_v4(), uuid_v4());
        let title = rules::title_seed(&text);
        let payload = commands::launch_thread(&LaunchThread {
            command_id: &command_id,
            thread_id: &thread_id,
            project_id: &project_id,
            title: &title,
            model_selection: &selection,
            runtime_mode: resolved.runtime_mode,
            interaction_mode: resolved.interaction_mode,
            message_id: &message_id,
            text: &text,
        });
        let request = self.state.read(cx).rpc(method::LAUNCH_THREAD, payload);
        self.begin_send(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(_) if intent == SubmitIntent::Background => {}
                    Ok(_) => this.state.update(cx, |state, cx| state.select_when_listed(&thread_id, cx)),
                    Err(error) => this.restore(&text, error.to_string(), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, thread_id: String, text: String, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        let Some(selection) = Self::dispatch_selection(&resolved) else {
            self.error = Some("Enable a provider in Settings to send a message".into());
            cx.notify();
            return;
        };
        let settings = self.state.read(cx).client_settings;
        let approvals = self.snapshot.pending.approvals.clone();
        let follow_up = if approvals.is_empty() { settings.follow_up_behavior } else { FollowUpBehavior::Steer };
        let dispatch = rules::dispatch_mode(self.snapshot.running, intent == SubmitIntent::Alternate, follow_up, true);
        let feedback = approvals.first().filter(|approval| approval.supports_feedback).map(|approval| approval.request_id.clone());
        let mut steps: Vec<(Value, &'static str)> = Vec::new();
        if let Some(request_id) = &feedback {
            let command = commands::respond_to_approval(&uuid_v4(), &thread_id, request_id, ApprovalDecision::Decline, Some(&text));
            steps.push((command, "Failed to submit approval decision."));
        } else {
            for approval in approvals.iter().filter(|approval| approval.live) {
                let command = commands::respond_to_approval(&uuid_v4(), &thread_id, &approval.request_id, ApprovalDecision::Decline, None);
                steps.push((command, "Failed to submit approval decision."));
            }
            if Some(resolved.runtime_mode) != self.snapshot.thread_runtime {
                steps.push((commands::set_runtime_mode(&uuid_v4(), &thread_id, resolved.runtime_mode), "Failed to send message."));
            }
            if Some(resolved.interaction_mode) != self.snapshot.thread_interaction {
                steps.push((commands::set_interaction_mode(&uuid_v4(), &thread_id, resolved.interaction_mode), "Failed to send message."));
            }
            let (command_id, message_id) = (uuid_v4(), uuid_v4());
            let title = rules::title_seed(&text);
            let message = ComposerMessage {
                command_id: &command_id,
                thread_id: &thread_id,
                message_id: &message_id,
                text: &text,
                title_seed: &title,
                model_selection: &selection,
                dispatch,
            };
            steps.push((commands::composer_message(&message), "Failed to send message."));
        }
        let requests: Vec<_> = steps
            .into_iter()
            .map(|(command, failure)| (self.state.read(cx).rpc(method::DISPATCH_COMMAND, command), failure))
            .collect();
        self.begin_send(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for (request, message) in requests {
                if let Err(error) = request.await {
                    failure = Some(format!("{message} {error}"));
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                if let Some(error) = failure {
                    this.restore(&text, error, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sending = true;
        self.error = None;
        self.set_prompt("", window, cx);
        cx.notify();
    }

    /// A failed send puts the text back unless the user already typed something new.
    fn restore(&mut self, text: &str, error: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt(cx).trim().is_empty() {
            self.set_prompt(text, window, cx);
        }
        self.error = Some(error);
    }

    fn respond_to_approval(&mut self, request_id: String, decision: ApprovalDecision, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        if !self.snapshot.pending.approvals.iter().any(|approval| approval.request_id == request_id && approval.live) {
            return;
        }
        if !self.responding.insert(request_id.clone()) {
            return;
        }
        self.approval_menu_open = false;
        let command = commands::respond_to_approval(&uuid_v4(), &thread_id, &request_id, decision, None);
        self.run_request(request_id, command, "Failed to submit approval decision.", cx);
    }

    fn run_request(&mut self, request_id: String, command: Value, failure: &'static str, cx: &mut Context<Self>) {
        let request = self.state.read(cx).rpc(method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.responding.remove(&request_id);
                if let Err(error) = result {
                    this.error = Some(format!("{failure} {error}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn rules_placeholder() -> &'static str {
    view::DEFAULT_PLACEHOLDER
}

/// The textarea's interval for single-select answers to settle before advancing (`200ms`).
const AUTO_ADVANCE: Duration = Duration::from_millis(200);
