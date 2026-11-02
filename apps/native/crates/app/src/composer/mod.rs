//! Composer: the prompt, its footer controls, and the panels for what the agent waits on. Ports
//! `ChatComposer.tsx` (layout, placeholders), `useComposerSend.ts` (send, launch, the approval
//! redirect), `usePendingThreadRequests.ts` (answering), `useComposerSelection.ts` (per-thread
//! model, permission and mode choices) and `composerCommandKey.ts` (keys). Choices persist the
//! way the web persists them: permissions and mode with their `thread.*.set` commands before the
//! next turn, the model on the turn itself.

mod attachments;
pub(crate) mod controls;
mod drafts;
mod menu;
mod model_picker;
mod pending;
mod queue;
mod send;
mod stash;
pub(crate) use pending::banner_surface;
mod view;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, EntityInputHandler, FocusHandle, KeyBinding, Subscription, Task, Window, actions, prelude::*};
use gpui_component::input::{InputEvent, OutdentInline, TextareaState};
use t3_core::composer::{self as rules, EnterKey};
use t3_core::lists;
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::requests::{self, DraftAnswer, PendingKey, PendingRequests};
use t3_core::server_config::{InteractionMode, OptionDescriptor, RuntimeMode};
use t3_core::traits::{self, OptionSelection};
use t3_core::workflows;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::provider_icon::ProviderMark;
use crate::sidebar::controller::now_ms;
use menu::{Menu, MenuKey};
use model_picker::{ModelPicker, PickerEvent, PickerInput};

actions!(composer, [TogglePlanMode, ToggleModelPicker, OpenPermissions, PasteAsText, StashPrompt, FocusPrompt]);

pub fn init(cx: &mut App) {
    // `modelPicker.toggle`, `composer.effort` and `composer.mode` bind through the keymap.
    cx.bind_keys([
        KeyBinding::new("shift-tab", TogglePlanMode, Some("Composer > Input")),
        // Keeps a large paste inline instead of attaching it (`isPasteAsTextShortcut`).
        KeyBinding::new("cmd-shift-v", PasteAsText, Some("Composer > Input")),
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
    /// A `/compact` turn is running, which a send cannot steer into.
    compacting: bool,
    /// The thread's projection has arrived, so `pending` is authoritative.
    loaded: bool,
    pending_key: PendingKey,
    pending: PendingRequests,
    /// Messages waiting behind the active turn.
    queue: t3_core::composer::queue::QueueState,
}

/// Everything the footer and send read, resolved against the server config.
#[derive(Debug, Clone, Default)]
struct Resolved {
    model: Option<ModelSelection>,
    model_label: String,
    driver: String,
    mark: Option<ProviderMark>,
    descriptors: Vec<OptionDescriptor>,
    /// The options a turn carries (`modelOptionsForDispatch`).
    dispatch_options: Vec<OptionSelection>,
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
    /// Whether the last Enter key-down was an OS key repeat. Recorded by the key listener, which
    /// runs before the textarea's `PressEnter` event reaches the composer.
    enter_repeat: bool,
    picker_dismissed: Option<Instant>,
    menu_dismissed: Option<Instant>,
    input_state: Option<(&'static str, bool)>,
    /// The question banner takes focus for its digit shortcuts while the prompt cannot type.
    question_focus: FocusHandle,
    limit_recovery: Entity<crate::thread_view::LimitRecoveryBanner>,
    /// A side chat's own thread, in the right panel; `None` follows the selection and draft.
    pub(super) fixed: Option<String>,
    /// The `@`, `/` and `$` menus.
    menu: Menu,
    /// Staged images and files per target.
    attachments: attachments::Attachments,
    drafts: drafts::Drafts,
    queue: queue::Queue,
    stash_state: stash::Stashed,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_for(state, None, window, cx)
    }

    /// The composer of `thread_id` whatever is selected, for a side chat tab.
    pub fn embedded(state: Entity<AppState>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_for(state, Some(thread_id), window, cx)
    }

    fn new_for(state: Entity<AppState>, fixed: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::SideThread) {
                    this.refresh(*event == AppEvent::ServerConfig, window, cx);
                }
                if *event == AppEvent::ComposerMention {
                    let mentions = this.state.update(cx, |state, _| std::mem::take(&mut state.composer_mentions));
                    for mention in mentions {
                        this.insert_mention(&mention.path, window, cx);
                    }
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::ComposerRestore {
                    this.restore_prompt(window, cx);
                }
            }),
            cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| this.on_picker(event, window, cx)),
            // Caret moves change the token under it; blinks find the caret unchanged and stop.
            cx.observe(&input, |this, _, cx| this.sync_menu(false, cx)),
        ];
        let limit_recovery = cx.new(|cx| crate::thread_view::LimitRecoveryBanner::new(state.clone(), cx));
        let mut composer = Composer {
            limit_recovery,
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
            enter_repeat: false,
            picker_dismissed: None,
            menu_dismissed: None,
            input_state: None,
            question_focus: cx.focus_handle(),
            fixed,
            menu: Menu::default(),
            attachments: Default::default(),
            drafts: Default::default(),
            queue: Default::default(),
            stash_state: Default::default(),
            _subscriptions: subscriptions,
        };
        composer.load_drafts();
        composer.load_stash();
        composer.refresh(true, window, cx);
        composer
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn read_snapshot(&self, cx: &App) -> Snapshot {
        let state = self.state.read(cx);
        let target = match (self.fixed.as_ref().or(state.selected_thread.as_ref()), &state.draft_project) {
            (Some(id), _) => Some(Target::Thread(id.clone())),
            (None, Some(project)) => Some(Target::Draft(project.clone())),
            _ => None,
        };
        let Some(Target::Thread(thread_id)) = &target else {
            let project_id = state.draft_project.clone();
            return Snapshot { target, project_id, ..Snapshot::default() };
        };
        let shell = state.thread_shell(thread_id);
        let projection = state.thread_state(thread_id).projection.as_ref().filter(|projection| projection.thread.id == *thread_id);
        let pending_key = projection.map(PendingKey::of).unwrap_or_default();
        let pending = if pending_key == self.snapshot.pending_key {
            self.snapshot.pending.clone()
        } else {
            projection.map(requests::pending_requests).unwrap_or_default()
        };
        let running = state.is_thread_running(thread_id);
        Snapshot {
            project_id: shell.map(|thread| thread.project_id.clone()),
            running,
            thread_model: shell.map(|thread| thread.model_selection.clone()),
            thread_runtime: shell.and_then(|thread| RuntimeMode::parse(&thread.runtime_mode)),
            thread_interaction: shell.and_then(|thread| InteractionMode::parse(&thread.interaction_mode)),
            started: projection.is_some_and(|projection| !projection.runs.is_empty()),
            handoff: projection.is_some_and(workflows::supports_provider_handoff),
            compacting: running
                && projection.is_some_and(|projection| {
                    workflows::is_compacting(projection, workflows::active_run(projection).map(|run| run.id.as_str()))
                }),
            loaded: projection.is_some(),
            pending_key,
            pending,
            queue: projection.map(t3_core::composer::queue::queue_state).unwrap_or_default(),
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
            let previous = self.snapshot.target.clone();
            self.switch_draft(previous.as_ref(), next.target.as_ref(), window, cx);
            self.picker_open = false;
            self.permissions_open = false;
            self.approval_menu_open = false;
            self.error = None;
            self.auto_advance = None;
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
        self.sync_queued_edit(window, cx);
        self.sync_answering(window, cx);
        self.sync_menu(true, cx);
        // The provider, its skills or the compact rule may have changed under the open menu.
        if self.menu_open() {
            self.refresh_menu(cx);
        }
        if self.picker_open {
            let input = self.picker_input(cx);
            self.picker.update(cx, |picker, cx| picker.update_input(input, config_changed, cx));
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
                Some(rules::draft_defaults(config, project, self.carry.0.as_ref(), self.carry.1, now_ms()))
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
        let mark = model
            .as_ref()
            .and_then(|model| entries.iter().find(|entry| entry.instance_id() == model.instance_id))
            .map(|entry| ProviderMark::of(entry, &entries));
        let provider = model.as_ref().and_then(|model| config?.provider(&model.instance_id));
        let provider_model = model.as_ref().and_then(|selection| provider?.models.iter().find(|m| m.slug == selection.model));
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
        let driver = provider.map(|p| p.driver.clone()).unwrap_or_default();
        let explicit = traits::selections(model.as_ref().and_then(|m| m.options.as_ref()));
        let options = traits::composer_options(&driver, provider_model, plan_enabled, &explicit);
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
            driver,
            mark,
            model,
            descriptors: options.descriptors,
            dispatch_options: options.dispatch,
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
                self.draft_changed(cx);
            }
            PickerEvent::Options(selections) => {
                self.set_options(selections, cx);
                self.draft_changed(cx);
            }
            PickerEvent::RuntimeMode(mode) => self.set_runtime_mode(*mode, cx),
            PickerEvent::Dismiss => {
                self.picker_open = false;
                self.focus(window, cx);
            }
        }
        if self.picker_open {
            let input = self.picker_input(cx);
            self.picker.update(cx, |picker, cx| picker.update_input(input, false, cx));
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
        self.draft_changed(cx);
        self.permissions_open = false;
        cx.notify();
    }

    /// Shift+Tab: outdent the list item under the caret, else toggle plan mode.
    fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let edit = {
            let input = self.input.read(cx);
            let range = input.selected_range();
            lists::outdent_for_tab(&input.value(), range.start, range.end)
        };
        if let Some(edit) = edit {
            self.input.update(cx, |input, cx| {
                input.set_selected_range(edit.start..edit.end, cx);
                input.replace(edit.replacement, window, cx);
                input.set_selected_range(edit.cursor_after..edit.cursor_after, cx);
            });
            return;
        }
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
        self.draft_changed(cx);
        cx.notify();
    }

    fn prompt(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn set_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.has_text = !text.trim().is_empty();
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
    }

    /// "Edit from here": the restored prompt joins the draft (`rewindToMessage`), cursor at the end.
    fn restore_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        let Some(restored) = self.state.update(cx, |state, _| state.take_composer_restore(&thread_id)) else { return };
        let prompt = t3_core::timeline::merge_restored_prompt(&self.prompt(cx), &restored);
        self.set_prompt(&prompt, window, cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn on_enter(&mut self, modifier: bool, window: &mut Window, cx: &mut Context<Self>) {
        let composing = self.input.update(cx, |input, cx| input.marked_text_range(window, cx).is_some());
        let key = EnterKey { modifier, repeat: std::mem::take(&mut self.enter_repeat), composing };
        let prompt = self.prompt(cx);
        let settings = self.state.read(cx).client_settings;
        let draft = matches!(self.snapshot.target, Some(Target::Draft(_)));
        match rules::submit_intent(key, self.snapshot.running, draft, settings.send_shortcut, &prompt) {
            Some(intent) => self.submit(intent, window, cx),
            // The input method commits its composition on Enter.
            None if composing => {}
            None => self.input.update(cx, |input, cx| input.insert("\n", window, cx)),
        }
    }

    /// The textarea draws itself; the composer repaints only when what it shows depends on the
    /// text: the send button's enabled state, or the active answer.
    fn on_prompt_change(&mut self, cx: &mut Context<Self>) {
        self.sync_menu(true, cx);
        self.close_stash_menu(cx);
        if self.answering.is_none() {
            self.draft_changed(cx);
        }
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
}

fn rules_placeholder() -> &'static str {
    view::DEFAULT_PLACEHOLDER
}

/// The textarea's interval for single-select answers to settle before advancing (`200ms`).
const AUTO_ADVANCE: Duration = Duration::from_millis(200);
