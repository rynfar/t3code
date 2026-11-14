//! Composer: the prompt, its footer controls, and the panels for what the agent waits on. Ports
//! `ChatComposer.tsx` (layout, placeholders), `useComposerSend.ts` (send, launch, the approval
//! redirect), `usePendingThreadRequests.ts` (answering), `useComposerSelection.ts` (per-thread
//! model, permission and mode choices) and `composerCommandKey.ts` (keys). Choices persist the
//! way the web persists them: permissions and mode with their `thread.*.set` commands before the
//! next turn, the model on the turn itself.

mod attachments;
mod branch_mismatch;
#[cfg(all(test, feature = "snapshot"))]
pub(crate) use branch_mismatch::CheckoutStatusOverride;
mod question_attachments;
#[cfg(feature = "snapshot")]
pub(crate) use question_attachments::QuestionCommandSpy;
#[cfg(feature = "snapshot")]
pub(crate) use attachments::Fixture as AttachmentFixture;
pub(crate) mod imports;
mod citations;
pub(crate) mod context;
pub(crate) mod clipboard;
pub(crate) mod controls;
pub(crate) mod drafts;
mod menu;
mod model_picker;
mod pending;
pub(crate) mod prompt_links;
mod queue;
mod send;
mod stash;
pub(crate) use pending::{banner_surface, banner_surface_card};
mod view;
mod workspace;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, KeyBinding, Subscription, Task, Window, actions, prelude::*};
use gpui_component::input::{InputEvent, OutdentInline, TextareaState};
use t3_core::composer::{self as rules, EnterKey};
use t3_core::lists;
use t3_core::model::ModelSelection;
use t3_core::providers;
use t3_core::requests::{self, DraftAnswer, PendingKey, PendingRequests};
use t3_core::server_config::{InteractionMode, OptionDescriptor, RuntimeMode};
use t3_core::settings::secrets::redacted_placeholder;
use t3_core::traits::{self, OptionSelection};
use t3_core::usage::{self, UsageProvider, limits as usage_limits};
use t3_core::workflows;

use crate::app_state::{AppEvent, AppState};
use crate::icons::Icon;
use crate::provider_icon::ProviderMark;
use crate::sidebar::controller::now_ms;
use crate::theme::{Palette, hsla, palette};
use crate::toast::{Toast, ToastKind};
use menu::{Menu, MenuKey};
use model_picker::{ModelPicker, PickerEvent, PickerInput};

actions!(composer, [TogglePlanMode, ToggleModelPicker, OpenPermissions, PasteAsText, StashPrompt, FocusPrompt, OpenWorkspace, UsePreviousWorktree]);

pub fn init(cx: &mut App) {
    // `modelPicker.toggle`, `composer.effort` and `composer.mode` bind through the keymap.
    cx.bind_keys([
        KeyBinding::new("cmd-c", gpui_component::input::Copy, Some("ContextChip")),
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
    Question(requests::attachments::QuestionDraftKey),
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
    environment: String,
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
    /// Worktree setup holds the send back (`worktreeSetupBlocksSend`).
    setup_blocks_send: bool,
    /// The run is being prepared, so a send queues behind it (`sendQueuesBehindSetup`).
    setup_queues: bool,
    /// Latest actionable proposed plan on a settled run (`findLatestProposedPlan`).
    active_proposed_plan: Option<t3_core::timeline::plan::ActiveProposedPlan>,
    /// Whether the thread currently offers a resume primary action (`resumableRunId !== null || hasHeldQueuedRuns`).
    can_resume: bool,
    last_error: Option<String>,
}

/// The corresponding transcript follows this composer's effective selection (`ChatView.tsx`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProviderTarget {
    pub environment: String,
    pub connection: Option<u64>,
    pub thread: Option<String>,
    pub instance: Option<String>,
}

impl ProviderTarget {
    pub(crate) fn environment(state: &AppState) -> String {
        state.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()).filter(|id| !id.is_empty()).or_else(|| state.environment().map(str::to_owned)).unwrap_or_default()
    }
}

impl EventEmitter<ProviderTarget> for Composer {}

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
    plan_menu_open: bool,
    choices: HashMap<Target, Choices>,
    /// The model and mode a new draft starts from: what the thread before it ran.
    carry: (Option<ModelSelection>, Option<InteractionMode>),
    snapshot: Snapshot,
    pub(crate) provider_target: ProviderTarget,
    provider_choice: Option<String>,
    responding: HashSet<String>,
    answers: HashMap<String, Answers>,
    /// The request whose active question the prompt edits, and the prompt it set aside.
    answering: Option<String>,
    question_collapsed: Option<String>,
    question_markdown: crate::transcript::markdown::MarkdownStore,
    stash: Option<String>,
    sending: bool,
    pending_send: t3_core::composer::attachments::PendingSend,
    pending_questions: Vec<(String, String)>,
    send_connection: Option<(Option<u64>, Option<u64>, crate::app_state::Phase)>,
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
    usage_limits: Option<UsageLimitsPanel>,
    checkout_vcs: Option<branch_mismatch::CheckoutWatcher>,
    revealed_branch_mismatch_key: Option<String>,
    restoring_branch_threads: HashSet<String>,
    /// A side chat's own thread, in the right panel; `None` follows the selection and draft.
    pub(super) fixed: Option<String>,
    /// The `@`, `/` and `$` menus.
    menu: Menu,
    /// Staged images and files per target.
    attachments: attachments::Attachments,
    /// Terminal excerpts per target and the chip row's popover.
    context: context::ContextChips,
    citation_editor: Option<citations::CommentEditor>,
    drafts: drafts::Drafts,
    workspace: workspace::Workspace,
    queue: queue::Queue,
    stash_state: stash::Stashed,
    /// The open thread's worktree setup; a side chat's composer has none.
    setup: Option<Entity<crate::worktree_setup::WorktreeSetupStore>>,
    _subscriptions: Vec<Subscription>,
}

#[cfg(feature = "snapshot")]
#[derive(Default)]
pub(crate) struct SnapshotAudit { pub submissions: std::cell::Cell<usize> }
#[cfg(feature = "snapshot")]
impl gpui::Global for SnapshotAudit {}

#[derive(Default)]
struct RegisteredComposers(Vec<gpui::WeakEntity<Composer>>);
impl gpui::Global for RegisteredComposers {}

#[cfg(feature = "snapshot")]
struct SnapshotComposer(Entity<Composer>);
#[cfg(feature = "snapshot")]
impl gpui::Global for SnapshotComposer {}
#[cfg(feature = "snapshot")]
pub(crate) fn snapshot_composer(cx: &App) -> Entity<Composer> {cx.global::<SnapshotComposer>().0.clone()}

pub(crate) fn insert_artifact_template(
    thread_id: Option<&str>,
    template: &t3_core::timeline::CodexArtifactTemplate,
    window: &mut Window,
    cx: &mut App,
) {
    let live: Vec<Entity<Composer>> = cx
        .try_global::<RegisteredComposers>()
        .map(|reg| reg.0.iter().filter_map(|weak| weak.upgrade()).collect())
        .unwrap_or_default();
    let target = match thread_id {
        Some(tid) => live
            .iter()
            .find(|composer| composer.read(cx).thread_id().as_deref() == Some(tid))
            .cloned(),
        None => live.iter().find(|composer| composer.read(cx).fixed.is_none()).cloned().or_else(|| live.first().cloned()),
    };
    if let Some(composer) = target {
        composer.update(cx, |composer, cx| composer.insert_artifact_template(template, window, cx));
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RewindComposerPrecheck {
    pub send_busy: bool,
    pub pending_attachments: bool,
    pub staged_count: usize,
}

pub(crate) fn rewind_precheck(thread_id: &str, cx: &App) -> RewindComposerPrecheck {
    let target = Target::Thread(thread_id.to_owned());
    let Some(reg) = cx.try_global::<RegisteredComposers>() else {
        return RewindComposerPrecheck::default();
    };
    let mut out = RewindComposerPrecheck::default();
    for weak in &reg.0 {
        let Some(entity) = weak.upgrade() else { continue };
        let composer = entity.read(cx);
        if composer.thread_id().as_deref() == Some(thread_id) {
            out.send_busy |= composer.sending || composer.pending_send.is_pending();
            out.pending_attachments |= composer.has_pending_attachments(&target);
            out.staged_count = out.staged_count.max(composer.staged_for(&target).len());
        }
    }
    out
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
        let setup = fixed.is_none().then(|| crate::worktree_setup::store(&state, cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { secondary, shift: false } => this.on_enter(*secondary, window, cx),
                InputEvent::Change => this.on_prompt_change(cx),
                _ => {}
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::Selection && this.fixed.is_none() && this.cancel_pending_send() { cx.notify(); }
                if *event == AppEvent::Phase {
                    let state = this.state.read(cx);
                    let connection = (state.connection_id(), state.client_handle().map(|client| *client.connection_generation().borrow()), state.phase.clone());
                    if this.send_connection.as_ref() != Some(&connection) {
                        this.send_connection = Some(connection);
                        if this.cancel_pending_send() { cx.notify(); }
                    }
                }
                if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::SideThread | AppEvent::Phase) {
                    this.refresh(*event == AppEvent::ServerConfig, window, cx);
                }
                if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase | AppEvent::ComposerFileDrop) {
                    this.consume_pending_file_drop(window, cx);
                }
                if matches!(event, AppEvent::Selection | AppEvent::Shell | AppEvent::ServerConfig | AppEvent::Phase) {
                    this.sync_workspace(*event == AppEvent::ServerConfig, cx);
                    this.sync_menu(false, cx);
                }
                if matches!(event, AppEvent::SidebarSelection | AppEvent::Shell | AppEvent::Phase) && !this.context.linked.is_empty() { cx.notify(); }
                if *event == AppEvent::BranchDraft && this.fixed.is_none() {
                    this.apply_branch_draft(window, cx);
                }
                if *event == AppEvent::ComposerMention {
                    let mentions = this.state.update(cx, |state, _| std::mem::take(&mut state.composer_mentions));
                    for mention in mentions {
                        this.insert_mention(&mention.path, window, cx);
                    }
                }
            }),
            cx.subscribe_in(&state, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::ComposerCitation
                    && let Some(thread_id) = this.thread_id()
                    && this.state.read(cx).composer_citation.as_ref().is_some_and(|c| c.thread_id == thread_id)
                    && let Some(citation) = this.state.update(cx, |state, _| state.composer_citation.take())
                {
                    this.add_citation(citation, window, cx);
                }
                if *event == AppEvent::ComposerRestore {
                    this.restore_prompt(window, cx);
                }
                if *event == AppEvent::Rewind {
                    if this.is_rewinding(cx) {
                        let _ = this.cancel_pending_send();
                        this.picker_open = false;
                        this.permissions_open = false;
                        this.plan_menu_open = false;
                        this.workspace.open = false;
                        this.close_stash_menu(cx);
                        let _ = this.close_context_details(cx);
                    }
                    this.sync_input_state(window, cx);
                    this.restore_prompt(window, cx);
                    cx.notify();
                }
                if *event == AppEvent::RemoveReviewComment
                    && let Some(thread_id) = this.thread_id()
                    && let Some(id) = this.state.update(cx, |state, _| state.take_removed_review_comment(&thread_id))
                {
                    this.remove_review_comment(&id, window, cx);
                }
                if *event == AppEvent::ComposerReviewComment
                    && let Some(thread_id) = this.thread_id()
                    && let Some(comment) = this.state.update(cx, |state, _| state.take_review_comment(&thread_id))
                {
                    this.add_review_comment(comment, window, cx);
                }
                if *event == AppEvent::ComposerTerminalContext
                    && let Some(thread_id) = this.thread_id()
                    && let Some(selection) = this.state.update(cx, |state, _| state.take_terminal_context(&thread_id))
                {
                    this.add_terminal_context(selection, window, cx);
                }
            }),
            cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| this.on_picker(event, window, cx)),
            // Caret moves change the token under it; blinks find the caret unchanged and stop.
            cx.observe(&input, |this, _, cx| this.sync_menu(false, cx)),
        ];
        if let Some(setup) = &setup {
            subscriptions.push(cx.subscribe_in(setup, window, |this, _, _: &crate::worktree_setup::SetupEvent, window, cx| this.refresh(false, window, cx)));
        }
        let limit_recovery = cx.new(|cx| crate::thread_view::LimitRecoveryBanner::new(state.clone(), cx));
        let mut composer = Composer {
            limit_recovery,
            usage_limits: None,
            checkout_vcs: None,
            revealed_branch_mismatch_key: None,
            restoring_branch_threads: HashSet::new(),
            state,
            input,
            picker,
            picker_open: false,
            permissions_open: false,
            plan_menu_open: false,
            choices: HashMap::new(),
            carry: (None, None),
            snapshot: Snapshot::default(),
            provider_target: ProviderTarget::default(),
            provider_choice: None,
            responding: HashSet::new(),
            answers: HashMap::new(),
            answering: None,
            question_collapsed: None,
            question_markdown: crate::transcript::markdown::MarkdownStore::new(cx.entity_id()),
            stash: None,
            sending: false,
            pending_send: Default::default(),
            pending_questions: Vec::new(),
            send_connection: None,
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
            context: Default::default(),
            citation_editor: None,
            drafts: Default::default(),
            workspace: Default::default(),
            queue: Default::default(),
            stash_state: Default::default(),
            setup,
            _subscriptions: subscriptions,
        };
        let weak_composer = cx.entity().downgrade();
        cx.update_default_global::<RegisteredComposers, _>(|reg, _cx| {
            reg.0.retain(|weak| weak.upgrade().is_some());
            reg.0.push(weak_composer);
        });
        #[cfg(feature = "snapshot")]
        if composer.fixed.is_none() {cx.set_global(SnapshotComposer(cx.entity()));}
        composer.load_drafts(cx);
        composer.load_stash();
        composer.refresh(true, window, cx);
        composer.sync_workspace(true, cx);
        composer
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn read_snapshot(&self, cx: &App) -> Snapshot {
        let state = self.state.read(cx);
        let environment = ProviderTarget::environment(state);
        let target = match (self.fixed.as_ref().or(state.selected_thread.as_ref()), &state.draft_project) {
            (Some(id), _) => Some(Target::Thread(id.clone())),
            (None, Some(project)) => Some(Target::Draft(project.clone())),
            _ => None,
        };
        let last_error = state.last_error.clone();
        let Some(Target::Thread(thread_id)) = &target else {
            let project_id = state.draft_project.clone();
            return Snapshot { environment, target, project_id, last_error, ..Snapshot::default() };
        };
        let shell = state.thread_shell(thread_id);
        let projection = state.thread_state(thread_id).projection.as_ref().filter(|projection| projection.thread.id == *thread_id);
        let pending_key = projection.map(PendingKey::of).unwrap_or_default();
        let pending = if pending_key == self.snapshot.pending_key && environment == self.snapshot.environment && target == self.snapshot.target {
            self.snapshot.pending.clone()
        } else {
            projection.map(requests::pending_requests).unwrap_or_default()
        };
        let running = state.is_thread_running(thread_id);
        let setup = self.setup.as_ref().and_then(|setup| setup.read(cx).view_for(Some(thread_id)));
        let (setup_blocks_send, setup_queues) = setup.map_or((false, false), |view| (view.blocks_send, view.queues_behind_setup));
        Snapshot {
            environment,
            setup_blocks_send,
            setup_queues,
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
            active_proposed_plan: projection.and_then(t3_core::timeline::plan::find_active_proposed_plan),
            can_resume: projection.is_some_and(t3_core::timeline::plan::can_resume_thread),
            last_error,
            target,
        }
    }

    pub(super) fn active_plan_follow_up(&self, cx: &App) -> Option<&t3_core::timeline::plan::ActiveProposedPlan> {
        let resolved = self.resolve(cx);
        let target = self.snapshot.target.as_ref();
        let has_attachments = target.is_some_and(|t| !self.staged_for(t).is_empty());
        let plan = self.snapshot.active_proposed_plan.as_ref();
        t3_core::timeline::plan::should_show_plan_follow_up_prompt(
            self.snapshot.pending.user_inputs.len(),
            resolved.interaction_mode,
            self.snapshot.running,
            plan,
            has_attachments,
        )
        .then_some(plan)
        .flatten()
    }

    /// Re-reads app state. Notifies only when something the composer shows changed.
    fn refresh(&mut self, config_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.watch_question_grants(cx);
        let next = self.read_snapshot(cx);
        let pruned = self.prune_question_attachments(&next.environment, cx);
        if self.pending_send.is_pending() && self.question_revision(cx) != self.pending_questions && self.cancel_pending_send() { cx.notify(); }
        if next.target != self.snapshot.target || next.pending.user_inputs != self.snapshot.pending.user_inputs {
            self.cancel_pending_send();
        }
        if next == self.snapshot && !config_changed {
            if self.provider_target.connection != self.state.read(cx).connection_id() { self.sync_provider_target(cx); }
            self.sync_checkout_vcs(cx);
            self.sync_branch_mismatch(cx);
            if self.finish_prepared_reads(cx) || pruned { cx.notify(); }
            return;
        }
        if next.target != self.snapshot.target || next.environment != self.snapshot.environment {
            if matches!(next.target, Some(Target::Draft(_))) && matches!(self.snapshot.target, Some(Target::Thread(_))) {
                let resolved = self.resolve(cx);
                self.carry = (resolved.model, Some(resolved.interaction_mode));
            }
            let previous = self.snapshot.target.clone();
            self.close_citation_comment("outside-press", window, cx);
            self.citation_editor = None;
            self.switch_draft(previous.as_ref(), next.target.as_ref(), window, cx);
            self.picker_open = false;
            self.permissions_open = false;
            self.error = None;
            self.auto_advance = None;
            if matches!(next.target, Some(Target::Draft(_))) {
                self.focus(window, cx);
            }
        }
        // `pruneResolvedUserInputDrafts`: only this thread's resolved drafts go; others wait.
        let pending_ids: Vec<&String> = next.pending.user_inputs.iter().map(|input| &input.request_id).collect();
        if let (Some(Target::Thread(thread_id)), true) = (&next.target, next.loaded) {
            self.answers.retain(|key, answers| {
                let same_environment = serde_json::from_str::<Vec<String>>(key).ok().is_some_and(|parts| parts.first() == Some(&next.environment));
                !same_environment || answers.thread_id != *thread_id || pending_ids.iter().any(|id| key == &Self::answer_key_for(&next.environment, thread_id, id))
            });
        }
        self.responding.retain(|request_id| {
            if let Ok(parts) = serde_json::from_str::<Vec<String>>(request_id) {
                return parts.len() == 3 && (!next.loaded || parts[0] != next.environment || Some(&parts[1]) != next.target.as_ref().and_then(|target| if let Target::Thread(id) = target { Some(id) } else { None }) || pending_ids.iter().any(|id| **id == parts[2]));
            }
            pending_ids.contains(&request_id)
        });
        self.snapshot = next;
        self.sync_provider_target(cx);
        self.sync_usage_limits(cx);
        self.sync_checkout_vcs(cx);
        self.sync_branch_mismatch(cx);
        self.finish_prepared_reads(cx);
        #[cfg(feature = "snapshot")]
        self.attachment_fixture(cx);
        self.sync_queued_edit(window, cx);
        self.sync_answering(window, cx);
        self.restore_prompt(window, cx);
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

    fn sync_provider_target(&mut self, cx: &mut Context<Self>) {
        self.provider_choice = self.choices().and_then(|choices| choices.model.as_ref()).map(|model| model.instance_id.clone());
        let next = ProviderTarget {
            environment: self.snapshot.environment.clone(),
            connection: self.state.read(cx).connection_id(),
            thread: self.thread_id(),
            instance: self.resolve(cx).model.map(|model| model.instance_id),
        };
        if next != self.provider_target {
            self.provider_target = next.clone();
            cx.emit(next);
        }
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

    pub(crate) fn resolved_provider_instance(&self, cx: &App) -> Option<String> {
        self.resolve(cx).model.map(|model| model.instance_id)
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
        if self.snapshot.target.is_none() || self.is_rewinding(cx) {
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
        if self.snapshot.target.is_none() || self.is_rewinding(cx) {
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
        if self.is_rewinding(cx) {
            self.picker_open = false;
            return;
        }
        match event {
            PickerEvent::Model { instance_id, model } => {
                let selection = ModelSelection { instance_id: instance_id.clone(), model: model.clone(), options: None };
                if let Some(choices) = self.choices_mut() {
                    choices.model = Some(selection);
                }
                self.draft_changed(cx);
                self.sync_usage_limits(cx);
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
        if self.is_rewinding(cx) {
            return;
        }
        if let Some(choices) = self.choices_mut() {
            choices.runtime_mode = Some(mode);
        }
        self.draft_changed(cx);
        self.permissions_open = false;
        cx.notify();
    }

    /// Shift+Tab: outdent the list item under the caret, else toggle plan mode.
    fn toggle_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
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

    pub(crate) fn prompt_text(&self, cx: &App) -> String {
        self.prompt(cx)
    }

    pub(crate) fn is_rewinding(&self, cx: &App) -> bool {
        self.thread_id().is_some_and(|id| self.state.read(cx).is_thread_rewinding(&id))
    }

    fn prompt(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn set_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_pending_send() { cx.notify(); }
        self.close_citation_comment("prompt-replaced", window, cx);
        self.has_text = !text.trim().is_empty();
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
    }

    /// "Edit from here": the restored prompt and attachments join the draft (`rewindToMessage`), cursor at the end.
    fn restore_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.answering.is_some() {
            return;
        }
        let Some(thread_id) = self.thread_id() else { return };
        let Some((restored, attachments)) = self.state.update(cx, |state, _| state.take_composer_restore(&thread_id)) else {
            return;
        };
        if !restored.is_empty() {
            let prompt = t3_core::timeline::merge_restored_prompt(&self.prompt(cx), &restored);
            self.set_prompt(&prompt, window, cx);
        }
        self.restore_reverted_attachments(&thread_id, attachments, cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn on_enter(&mut self, modifier: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.open && self.workspace_enabled() {return;}
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
        if self.cancel_pending_send() { cx.notify(); }
        self.sync_menu(true, cx);
        self.close_stash_menu(cx);
        if self.answering.is_none() {
            self.draft_changed(cx);
        }
        let text = self.prompt(cx);
        self.sync_citation_comment(&text, cx);
        self.sync_context_chips(&text, cx);
        self.sync_branch_mismatch(cx);
        let has_text = !text.trim().is_empty();
        let answering = self.active_question();
        if let Some((request_id, question)) = &answering {
            let thread_id = self.thread_id().unwrap_or_default();
            let answer_key = self.answer_key(request_id);
            let answers = self.answers.entry(answer_key).or_insert_with(|| Answers { thread_id, ..Answers::default() });
            let next = requests::set_custom_answer(question, answers.drafts.get(&question.id), &text);
            answers.drafts.insert(question.id.clone(), next);
        }
        if answering.is_some() || has_text != self.has_text {
            self.has_text = has_text;
            cx.notify();
        }
    }

    fn consume_pending_file_drop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fixed.is_some() {
            return;
        }
        let Some(thread_id) = self.thread_id() else { return };
        let Some(paths) = self.state.update(cx, |state, _| state.take_pending_composer_file_drop(&thread_id)) else {
            return;
        };
        if !self.can_attach(cx) {
            return;
        }
        self.attach_paths(paths, window, cx);
        self.focus(window, cx);
    }

    fn thread_id(&self) -> Option<String> {
        match &self.snapshot.target {
            Some(Target::Thread(id)) => Some(id.clone()),
            _ => None,
        }
    }

    pub(super) fn has_non_prompt_content(&self, prompt: &str, staged: &[t3_core::composer::attachments::Attachment]) -> bool {
        !staged.is_empty() || self.has_context_records(prompt)
    }

    pub(super) fn usage_limits_offered(&self, cx: &App) -> bool {
        let resolved = self.resolve(cx);
        let state = self.state.read(cx);
        let Some(config) = state.server_config.config.as_ref() else { return false };
        let Some(driver) = resolved.model.as_ref().and_then(|model| config.provider(&model.instance_id)).map(|provider| provider.driver.as_str()) else {
            return false;
        };
        usage_limits::has_provider_usage_limits(driver, &config.providers, &config.usage_limit_sources)
    }

    pub(super) fn usage_limits_key(&self, cx: &App) -> Option<String> {
        let resolved = self.resolve(cx);
        let instance_id = resolved.model.as_ref().map(|model| model.instance_id.as_str())?;
        if matches!(self.snapshot.target, Some(Target::Thread(_))) && !self.snapshot.loaded {
            return None;
        }
        let target = self.snapshot.target.as_ref()?;
        let target_key = drafts::key(target);
        let state = self.state.read(cx);
        let latest_run_id = match target {
            Target::Thread(id) => state.thread_shell(id).and_then(|shell| shell.latest_run_id.as_deref()).unwrap_or(""),
            Target::Draft(_) | Target::Question(_) => "",
        };
        let pending_id = self
            .snapshot
            .pending
            .approvals
            .first()
            .map(|approval| approval.request_id.as_str())
            .or_else(|| self.snapshot.pending.user_inputs.first().map(|input| input.request_id.as_str()))
            .unwrap_or("");
        Some(format!("{}:{target_key}:{instance_id}:{latest_run_id}:{pending_id}", self.snapshot.environment))
    }

    pub(super) fn can_intercept_usage_limits(&self, prompt: &str, cx: &App) -> bool {
        self.usage_limits_offered(cx)
            && self.usage_limits_key(cx).is_some()
            && !self.has_non_prompt_content(prompt, &self.staged_attachments())
    }

    pub(super) fn sync_usage_limits(&mut self, cx: &App) {
        let Some(panel) = &self.usage_limits else { return };
        if self.usage_limits_key(cx).as_deref().is_some_and(|key| key != panel.key) {
            self.usage_limits = None;
        }
    }

    pub(super) fn open_usage_limits(&mut self, cx: &mut Context<Self>) -> bool {
        let now_ms = crate::transcript::Clock::now().now_ms;
        let opened = (|| {
            let key = self.usage_limits_key(cx)?;
            let target = self.snapshot.target.clone()?;
            let instance_id = self.resolve(cx).model.map(|model| model.instance_id)?;
            let state = self.state.read(cx);
            let config = state.server_config.config.as_ref()?;
            usage_limits::collect_provider_usage_limits(&instance_id, &config.providers, &config.usage_limit_sources, now_ms)?;
            Some((key, target))
        })();
        if let Some((key, target)) = opened {
            self.usage_limits = Some(UsageLimitsPanel {
                key,
                target,
                now_ms,
                revealed: HashSet::new(),
                confirm_reset: None,
                reset_busy: None,
                reset_status: None,
            });
            cx.notify();
            return true;
        }
        self.usage_limits = None;
        self.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Info, "Usage limits are unavailable for this provider"), cx));
        cx.notify();
        false
    }

    pub(super) fn dismiss_usage_limits(&mut self, cx: &mut Context<Self>) {
        self.usage_limits = None;
        cx.notify();
    }

    fn clear_usage_limits_for(&mut self, target: &Target) {
        if self.usage_limits.as_ref().is_some_and(|panel| &panel.target == target) {
            self.usage_limits = None;
        }
    }

    fn redeem_usage_limit_credit(&mut self, account_id: String, input: usage_limits::ResetCreditInput, cx: &mut Context<Self>) {
        let can_manage = self
            .state
            .read(cx)
            .client_handle()
            .is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(usage::PROVIDERS_MANAGE_SCOPE)));
        if !can_manage {
            return;
        }
        let Some(panel) = self.usage_limits.as_mut() else { return };
        let panel_key = panel.key.clone();
        panel.confirm_reset = None;
        panel.reset_busy = Some(account_id.clone());
        panel.reset_status = None;
        let request = self.state.read(cx).rpc(usage::method::CONSUME_RESET_CREDIT, input.payload());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let text = match request.await {
                Ok(reply) => usage_limits::reset_outcome_text(&reply),
                Err(error) => {
                    let message = error.to_string();
                    if message.is_empty() { "Could not use the reset credit.".to_owned() } else { message }
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(panel) = this.usage_limits.as_mut().filter(|panel| panel.key == panel_key) {
                    if panel.reset_busy.as_deref() == Some(account_id.as_str()) {
                        panel.reset_busy = None;
                    }
                    panel.reset_status = Some((account_id, text));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_usage_account_summary(
        &self,
        account: &usage_limits::ReportAccount,
        id: gpui::SharedString,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use gpui_component::h_flex;
        let label = account.summary_label();
        let revealed = self.usage_limits.as_ref().is_some_and(|panel| panel.revealed.contains(&label));
        let label_el = if label.contains('@') {
            let shown = if revealed { label.clone() } else { redacted_placeholder(&label) };
            let key = label;
            gpui::div()
                .id(id)
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(if revealed { "Click to hide account" } else { "Click to reveal account" }).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(panel) = this.usage_limits.as_mut() {
                        if !panel.revealed.remove(&key) {
                            panel.revealed.insert(key.clone());
                        }
                        cx.notify();
                    }
                }))
                .child(shown)
                .into_any_element()
        } else {
            gpui::div().min_w_0().truncate().child(label).into_any_element()
        };
        h_flex()
            .min_w_0()
            .gap(gpui::px(4.))
            .text_color(hsla(p.muted_foreground))
            .child(label_el)
            .children(account.plan.as_deref().map(|plan| gpui::div().flex_none().child(format!("· {plan}"))))
            .into_any_element()
    }

    pub(super) fn render_usage_limits_banner(&self, detached: bool, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui::{FontWeight, div, px, relative};
        use gpui_component::{ActiveTheme, h_flex, v_flex};
        let panel = self.usage_limits.as_ref()?;
        let key = self.usage_limits_key(cx)?;
        if panel.key != key {
            return None;
        }
        let instance_id = self.resolve(cx).model.map(|model| model.instance_id)?;
        let report = {
            let state = self.state.read(cx);
            let config = state.server_config.config.as_ref()?;
            usage_limits::collect_provider_usage_limits(&instance_id, &config.providers, &config.usage_limit_sources, panel.now_ms)?
        };
        let p = palette(cx);
        let card_bg = hsla(if cx.theme().is_dark() { p.surface_raised } else { p.card });
        let clock = crate::transcript::Clock::now();
        let ts_format = crate::settings::store::client(cx).timestamp_format();
        let now = t3_core::timeline::parse_iso_ms(&report.created_at).unwrap_or(panel.now_ms);
        let can_manage = self
            .state
            .read(cx)
            .client_handle()
            .is_some_and(|client| client.session_state().borrow().as_ref().is_some_and(|session| session.grants(usage::PROVIDERS_MANAGE_SCOPE)));
        let summary = match report.accounts.as_slice() {
            [single] => self.render_usage_account_summary(single, "usage-limits-summary-label".into(), p, cx),
            accounts => div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(format!("{} accounts", accounts.len())).into_any_element(),
        };
        let header = h_flex()
            .w_full()
            .min_h(px(24.))
            .gap(px(4.))
            .child(div().flex_none().w(px(24.)).flex().justify_center().child(Icon::Gauge.render(px(12.), hsla(p.muted_foreground))))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .whitespace_nowrap()
                    .child(div().flex_none().font_weight(FontWeight::MEDIUM).line_height(px(24.)).child("Usage limits"))
                    .child(summary),
            )
            .child(crate::transcript::ghost_icon_button(
                "usage-limits-dismiss",
                Icon::X,
                cx.listener(|this, _, _, cx| this.dismiss_usage_limits(cx)),
                cx,
            ));

        let mut body = v_flex().gap(px(8.)).pt(px(4.)).pb(px(6.)).pl(px(28.)).pr(px(8.));
        for (account_ix, account) in report.accounts.iter().enumerate() {
            let mut block = v_flex().min_w_0().gap(px(4.));
            if report.accounts.len() > 1 {
                block = block.child(self.render_usage_account_summary(account, format!("usage-limits-account-{account_ix}").into(), p, cx));
            }
            if let Some(notice) = usage_limits::limits_notice(&account.limits) {
                block = block.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(notice));
            } else {
                let bar_color = usage_limits::accent_rgba(account.accent_color.as_deref())
                    .map(gpui::rgba)
                    .map(gpui::Hsla::from)
                    .unwrap_or_else(|| usage_bar_color(&account.driver, p));
                let mut label_col = v_flex().w(px(144.)).flex_none().gap(px(2.));
                let mut bar_col = v_flex().flex_1().min_w(px(48.)).gap(px(2.));
                let mut meta_col = v_flex().flex_none().gap(px(2.));
                for (win_ix, window) in account.limits.windows.iter().enumerate() {
                    let remaining = usage_limits::remaining_percent(window);
                    let pace = usage_limits::pace_of(window, now);
                    let pace_left = usage_limits::pace_remaining_percent(window, now);
                    let resets = usage_limits::format_resets_in(window, now);
                    let reset_at = window
                        .resets_at
                        .as_deref()
                        .map(|iso| t3_core::timeline::format_upcoming(iso, clock.now_ms, &clock.zone, ts_format, &crate::timestamp::SystemTimestampLocale))
                        .filter(|s| !s.is_empty());
                    let mut tip_lines = vec![match pace_left {
                        Some(left) => format!("{remaining}% left · {left}% of the window left"),
                        None => format!("{remaining}% left"),
                    }];
                    if pace_left.is_some() {
                        tip_lines.push("The line is where even spending would be.".to_owned());
                    }
                    if let (Some(at), Some(resets_in)) = (reset_at.as_deref(), resets.as_deref()) {
                        let suffix = resets_in.strip_prefix("resets ").or_else(|| resets_in.strip_prefix("Resets ")).unwrap_or(resets_in);
                        tip_lines.push(format!("Resets {at} · {suffix}"));
                    } else if let Some(resets_in) = resets.as_deref() {
                        tip_lines.push(resets_in.to_owned());
                    }
                    let bar_tip: gpui::SharedString = tip_lines.join("\n").into();
                    let pace_icon = pace.map(|pace| {
                        let glyph = match pace {
                            usage_limits::LimitPace::Ahead => Icon::TrendingUp,
                            usage_limits::LimitPace::On => Icon::Gauge,
                            usage_limits::LimitPace::Under => Icon::TrendingDown,
                        };
                        let label = gpui::SharedString::from(pace.label());
                        div()
                            .id(("usage-limits-pace", account_ix * 16 + win_ix))
                            .flex_none()
                            .child(glyph.render(px(12.), hsla(p.muted_foreground)))
                            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                    });
                    label_col = label_col.child(
                        h_flex()
                            .h(px(24.))
                            .items_center()
                            .min_w_0()
                            .gap(px(6.))
                            .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child(window.label.clone()))
                            .child(div().ml_auto().flex_none().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(format!("{remaining}% left"))),
                    );
                    bar_col = bar_col.child(
                        h_flex()
                            .id(("usage-limits-bar", account_ix * 16 + win_ix))
                            .h(px(24.))
                            .items_center()
                            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(bar_tip.clone()).build(window, cx))
                            .child(
                                div()
                                    .relative()
                                    .w_full()
                                    .h(px(14.))
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .relative()
                                            .w_full()
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(hsla(p.muted))
                                            .overflow_hidden()
                                            .when(remaining > 0, |this| this.child(div().h_full().rounded_full().bg(bar_color).w(relative(remaining as f32 / 100.0)))),
                                    )
                                    .when_some(pace_left, |this, pace_left| {
                                        this.child(
                                            div().absolute().top_0().bottom_0().left(px(3.)).right(px(3.)).child(
                                                div()
                                                    .absolute()
                                                    .top_0()
                                                    .bottom_0()
                                                    .left(relative(pace_left as f32 / 100.0))
                                                    .ml(px(-3.))
                                                    .w(px(6.))
                                                    .flex()
                                                    .justify_center()
                                                    .bg(card_bg)
                                                    .child(div().w(px(2.)).h_full().rounded_full().bg(hsla(p.foreground))),
                                            ),
                                        )
                                    }),
                            ),
                    );
                    meta_col = meta_col.child(
                        h_flex()
                            .h(px(24.))
                            .items_center()
                            .gap(px(6.))
                            .text_color(hsla(p.muted_foreground))
                            .whitespace_nowrap()
                            .children(pace_icon)
                            .children(resets.map(|text| div().child(text))),
                    );
                }
                if !account.limits.windows.is_empty() {
                    block = block.child(h_flex().w_full().min_w_0().items_start().gap(px(12.)).text_xs().child(label_col).child(bar_col).child(meta_col));
                }
            }
            if let Some(external) = &account.limits.external_usage {
                let url = external.url.clone();
                block = block.child(h_flex().child(crate::transcript::ghost_text_button(
                    ("usage-limits-manage", account_ix),
                    "Manage usage",
                    Some(Icon::ExternalLink),
                    move |_, _, cx| cx.open_url(&url),
                    cx,
                )));
            }
            if let Some(credits) = &account.limits.reset_credits {
                let status = panel.reset_status.as_ref().filter(|(id, _)| *id == account.id).map(|(_, msg)| msg.clone());
                if credits.available_count > 0 || status.is_some() {
                    let reset_input = account.reset_credit_input.clone().or_else(|| {
                        account.instance_id.clone().map(|instance_id| usage_limits::ResetCreditInput::Instance { instance_id })
                    });
                    let account_id = account.id.clone();
                    let busy = panel.reset_busy.as_deref() == Some(account.id.as_str());
                    let confirming = panel.confirm_reset.as_deref() == Some(account.id.as_str());
                    let mut credits_row = h_flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(usage_limits::reset_credits_summary(credits.available_count, credits.next_expires_at.as_deref(), now, false));
                    if credits.available_count > 0
                        && let Some(input) = reset_input
                    {
                        if confirming {
                            let confirm_id = account_id.clone();
                            credits_row = credits_row
                                .child(
                                    controls::Button::new(("usage-limits-confirm-reset", account_ix), controls::ButtonVariant::Default, controls::ButtonSize::Xs)
                                        .label("Use credit")
                                        .disabled(busy || !can_manage)
                                        .on_click(cx.listener(move |this, _, _, cx| this.redeem_usage_limit_credit(confirm_id.clone(), input.clone(), cx))),
                                )
                                .child(
                                    controls::Button::new(("usage-limits-cancel-reset", account_ix), controls::ButtonVariant::Ghost, controls::ButtonSize::Xs)
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(panel) = this.usage_limits.as_mut() {
                                                panel.confirm_reset = None;
                                                cx.notify();
                                            }
                                        })),
                                );
                        } else {
                            let start_id = account_id.clone();
                            credits_row = credits_row.child(
                                controls::Button::new(("usage-limits-use-reset", account_ix), controls::ButtonVariant::Outline, controls::ButtonSize::Xs)
                                    .label(if busy { "Using…" } else { "Use reset" })
                                    .disabled(busy || !can_manage)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(panel) = this.usage_limits.as_mut() {
                                            panel.confirm_reset = Some(start_id.clone());
                                            cx.notify();
                                        }
                                    })),
                            );
                        }
                    }
                    if let Some(status) = status {
                        credits_row = credits_row.child(div().text_color(hsla(p.foreground)).child(status));
                    }
                    block = block.child(credits_row);
                }
            }
            body = body.child(block);
        }
        for notice in &report.notices {
            body = body.child(div().text_xs().text_color(hsla(p.muted_foreground)).child(notice.clone()));
        }
        Some(
            banner_surface_card(false, detached, v_flex().child(header).child(div().id("usage-limits-scroll").max_h(px(240.)).overflow_y_scroll().child(body)), cx)
                .id("usage-limits-banner")
                .debug_selector(|| "usage-limits-banner".to_owned())
                .into_any_element(),
        )
    }
}

struct UsageLimitsPanel {
    key: String,
    target: Target,
    now_ms: i64,
    revealed: HashSet<String>,
    confirm_reset: Option<String>,
    reset_busy: Option<String>,
    reset_status: Option<(String, String)>,
}

fn usage_bar_color(driver: &str, p: Palette) -> gpui::Hsla {
    UsageProvider::from_driver(driver).map_or(hsla(p.foreground), |provider| {
        hsla(match provider {
            UsageProvider::Codex => p.usage_codex,
            UsageProvider::Claude => p.usage_claude,
            UsageProvider::Grok => p.usage_grok,
            UsageProvider::Cursor => p.usage_cursor,
            UsageProvider::Opencode => p.usage_opencode,
            UsageProvider::Antigravity => p.usage_antigravity,
            UsageProvider::Pi => p.usage_pi,
            UsageProvider::Muse => p.usage_muse,
        })
    })
}

fn rules_placeholder() -> &'static str {
    view::DEFAULT_PLACEHOLDER
}

/// The textarea's interval for single-select answers to settle before advancing (`200ms`).
const AUTO_ADVANCE: Duration = Duration::from_millis(200);

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn snapshot_audit(&self, submissions: usize, cx: &App) -> serde_json::Value {
        serde_json::json!({"submissions": submissions, "prompt": self.prompt(cx), "workspace": self.selected_workspace(), "menuCwd": self.menu_cwd(cx), "questionProgress": self.progress(cx).map(|progress| serde_json::json!({"index":progress.question_index,"complete":progress.is_complete,"advance":progress.can_advance})), "regularAttachmentCount": self.snapshot.target.as_ref().map_or(0, |target| self.staged_for(target).len()), "activeAttachmentCount":self.staged().len(), "regularPrompt":self.draft_prompt(cx)})
    }
}

#[cfg(all(test, feature = "snapshot"))]
impl Composer {
    pub(crate) fn pick_banner_test_model(&mut self, instance: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.on_picker(&PickerEvent::Model { instance_id: instance.into(), model: "m".into() }, window, cx);
    }

    pub(crate) fn restore_banner_test_model(&mut self, instance: &str, cx: &mut Context<Self>) {
        let target = self.snapshot.target.clone().unwrap();
        let draft = t3_core::composer::drafts::Draft { model_selection: Some(ModelSelection { instance_id: instance.into(), model: "m".into(), options: None }), ..Default::default() };
        self.restore_draft_choices(&target, &draft, cx);
    }

    pub(crate) fn type_banner_test_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_prompt("unsent text", window, cx);
        self.on_prompt_change(cx);
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod stream_error_tests {
    use super::*;
    use t3_client::supervisor::ClientEvent;

    #[gpui::test]
    fn stream_error_renders_and_clears_when_snapshot_is_otherwise_unchanged(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
            "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
            "projection": {
                "thread": {
                    "id": "t1", "projectId": "checkout-a", "title": "T1",
                    "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                    "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                    "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                    "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                },
                "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [], "updatedAt": "2026-04-01T00:00:05.000Z"
            }
        }"#).unwrap();
        state.update(cx, |state, cx| {
            state.select_thread("t1", cx);
            state.thread.apply(snapshot.clone());
            cx.emit(AppEvent::Thread);
        });

        let (composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));
        let notifications = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let obs_count = notifications.clone();
        let _obs = vcx.update(|_, cx| {
            cx.observe(&composer, move |_, _| {
                obs_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("composer-stream-error").is_none());

        let before_err = notifications.load(std::sync::atomic::Ordering::SeqCst);
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.apply_batch(
                    vec![ClientEvent::StreamError { context: "thread t1".into(), message: "stream boom".into() }],
                    cx,
                );
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(
            notifications.load(std::sync::atomic::Ordering::SeqCst) > before_err,
            "StreamError must notify Composer when snapshot is otherwise unchanged"
        );
        assert!(
            vcx.debug_bounds("composer-stream-error").is_some(),
            "StreamError must repaint Composer and render composer-stream-error"
        );
        composer.read_with(vcx, |composer, _| {
            assert_eq!(composer.snapshot.last_error.as_deref(), Some("Could not read thread t1: stream boom"));
        });

        let before_clear = notifications.load(std::sync::atomic::Ordering::SeqCst);
        vcx.update(|_, cx| {
            state.update(cx, |state, cx| {
                state.apply_batch(
                    vec![ClientEvent::Thread { thread_id: "t1".into(), item: Box::new(snapshot) }],
                    cx,
                );
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(
            notifications.load(std::sync::atomic::Ordering::SeqCst) > before_clear,
            "Thread item recovery must notify Composer to clear error"
        );
        assert!(
            vcx.debug_bounds("composer-stream-error").is_none(),
            "Thread item recovery must repaint Composer and clear composer-stream-error"
        );
        composer.read_with(vcx, |composer, _| {
            assert_eq!(composer.snapshot.last_error, None);
        });
    }

    #[gpui::test]
    fn usage_limits_command_opens_banner_and_clears_on_key_change(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            let config = state.server_config.config.as_mut().unwrap();
            config.providers = serde_json::from_value(serde_json::json!([
                {
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated", "label": "Plus" },
                    "models": [{ "slug": "m", "name": "GPT-5" }],
                    "usageLimits": {
                        "checkedAt": "2026-04-01T00:00:00.000Z",
                        "windows": [{ "id": "session", "kind": "session", "label": "5h", "usedPercent": 25 }]
                    },
                    "slashCommands": [{ "name": "usage-limits", "description": "Show usage limits" }]
                },
                {
                    "instanceId": "claudeAgent",
                    "driver": "claudeAgent",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated" },
                    "models": [{ "slug": "m", "name": "Sonnet" }]
                }
            ]))
            .unwrap();
            state.start_draft("checkout-a", cx);
            cx.emit(AppEvent::ServerConfig);
        });

        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let banner = self.0.update(cx, |composer, cx| composer.render_usage_limits_banner(false, cx));
                gpui::div().w(gpui::px(720.)).children(banner)
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("/usage-limits", window, cx);
                composer.submit(t3_core::composer::SubmitIntent::Foreground, window, cx);
                assert_eq!(composer.prompt(cx), "");
                assert!(composer.usage_limits.is_some());
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("usage-limits-banner").is_some());

        // Switching provider changes usageLimitsKey and closes the banner.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| composer.pick_banner_test_model("claudeAgent", window, cx));
            window.draw(cx).clear();
        });
        composer.read_with(vcx, |composer, _| assert!(composer.usage_limits.is_none()));
        assert!(vcx.debug_bounds("usage-limits-banner").is_none());

        // Calling open_usage_limits on a provider without limits shows the unavailability toast.
        vcx.update(|_, cx| {
            composer.update(cx, |composer, cx| {
                assert!(!composer.open_usage_limits(cx));
            });
        });
        state.read_with(vcx, |state, _| {
            assert_eq!(state.toasts.latest().map(|toast| toast.title.as_ref()), Some("Usage limits are unavailable for this provider"));
        });

        // Picking /usage-limits from the slash menu removes the trigger and opens the banner.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.pick_banner_test_model("codex", window, cx);
                composer.set_prompt("/usage", window, cx);
                composer.input.update(cx, |input, cx| input.set_selected_range(6..6, cx));
                composer.on_prompt_change(cx);
                let item = t3_core::composer::menu::MenuItem {
                    id: "cmd:usage-limits".into(),
                    label: "/usage-limits".into(),
                    description: String::new(),
                    action: t3_core::composer::menu::MenuAction::ProviderCommand { name: "usage-limits".into() },
                };
                composer.select_menu_item(item, window, cx);
                assert_eq!(composer.prompt(cx), "");
                assert!(composer.usage_limits.is_some());
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("usage-limits-banner").is_some());
    }

    #[gpui::test]
    fn usage_limits_runs_when_setup_blocks_send_and_draft_banner_is_out_of_flow(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            state.server_config.config = serde_json::from_value(serde_json::json!({
                "environment": { "environmentId": "env-1", "label": "Local" },
                "cwd": "/tmp",
                "providers": [{
                    "instanceId": "codex", "driver": "codex", "status": "ready", "enabled": true, "installed": true,
                    "auth": { "status": "authenticated" },
                    "usageLimits": {
                        "checkedAt": "2026-11-02T09:00:00Z",
                        "windows": [
                            { "id": "five_hour", "kind": "session", "label": "5h", "usedPercent": 20, "windowDurationMins": 300, "resetsAt": "2026-11-02T14:00:00Z" },
                            { "id": "weekly", "kind": "weekly", "label": "Weekly", "usedPercent": 80, "windowDurationMins": 10080, "resetsAt": "2026-11-09T09:00:00Z" }
                        ]
                    },
                    "models": [{ "slug": "m", "name": "GPT-5" }]
                }]
            }))
            .unwrap();
            state.start_draft("checkout-a", cx);
            cx.emit(AppEvent::ServerConfig);
        });

        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let draft_banner = self.0.update(cx, |composer, cx| composer.render_usage_limits_banner(true, cx));
                gpui_component::v_flex()
                    .w(gpui::px(720.))
                    .h(gpui::px(600.))
                    .justify_center()
                    .child(
                        gpui_component::v_flex()
                            .relative()
                            .w_full()
                            .child(gpui::div().id("composer-card").debug_selector(|| "composer-card".to_owned()).h(gpui::px(96.)))
                            .children(draft_banner.map(view::render_draft_usage_limits_overlay)),
                    )
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());

        vcx.update(|window, cx| window.draw(cx).clear());
        let before_card = vcx.debug_bounds("composer-card").expect("draft composer card");

        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.snapshot.setup_blocks_send = true;
                composer.set_prompt("/usage-limits", window, cx);
                composer.submit(t3_core::composer::SubmitIntent::Foreground, window, cx);
                assert_eq!(composer.prompt(cx), "");
                assert!(composer.usage_limits.is_some());
            });
            window.draw(cx).clear();
        });
        let after_card = vcx.debug_bounds("composer-card").expect("draft composer card after banner");
        assert_eq!(before_card.origin.y, after_card.origin.y);
        let overlay = vcx.debug_bounds("draft-usage-limits-overlay").expect("draft overlay");
        assert!(overlay.origin.y >= after_card.bottom());
        assert!(vcx.debug_bounds("usage-limits-banner").is_some());
    }

    #[gpui::test]
    fn branch_mismatch_banner_reveals_with_content_stays_sticky_and_dismisses(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        cx.update(|cx| {
            let mut overrides = std::collections::HashMap::new();
            overrides.insert(
                "/repo".to_owned(),
                t3_core::vcs::VcsStatus {
                    local: t3_core::vcs::VcsLocalStatus { ref_name: Some("main".to_owned()), ..Default::default() },
                    ..Default::default()
                },
            );
            cx.set_global(CheckoutStatusOverride(overrides));
        });
        state.update(cx, |state, cx| {
            state.shell.snapshot = serde_json::from_value(serde_json::json!({
                "snapshotSequence": 1,
                "projects": [{
                    "id": "p1", "title": "P1", "workspaceRoot": "/repo", "scripts": [],
                    "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"
                }],
                "threads": [
                    {
                        "id": "t1", "projectId": "p1", "title": "T1",
                        "providerInstanceId": "codex",
                        "modelSelection": { "instanceId": "codex", "model": "m" },
                        "runtimeMode": "full-access", "interactionMode": "default",
                        "status": "idle", "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                        "lineage": { "rootThreadId": "t1" },
                        "branch": "feat/a"
                    },
                    {
                        "id": "t2", "projectId": "p1", "title": "T2",
                        "providerInstanceId": "codex",
                        "modelSelection": { "instanceId": "codex", "model": "m" },
                        "runtimeMode": "full-access", "interactionMode": "default",
                        "status": "idle", "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                        "lineage": { "rootThreadId": "t2" },
                        "branch": "main"
                    }
                ]
            }))
            .unwrap();
            state.selected_thread = Some("t1".into());
            cx.emit(AppEvent::Shell);
            cx.emit(AppEvent::Selection);
        });

        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let banner = self.0.update(cx, |composer, cx| composer.render_branch_mismatch_banner(cx));
                gpui::div().w(gpui::px(720.)).children(banner)
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());

        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_none());

        // Typing reveals the banner and clearing keeps it sticky on the same thread/mismatch key.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("hello", window, cx);
                composer.on_prompt_change(cx);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_some());

        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("", window, cx);
                composer.on_prompt_change(cx);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_some());

        // Switching to another thread and back with empty prompt clears the sticky reveal.
        state.update(vcx, |state, cx| {
            state.selected_thread = Some("t2".into());
            cx.emit(AppEvent::Selection);
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_none());
        state.update(vcx, |state, cx| {
            state.selected_thread = Some("t1".into());
            cx.emit(AppEvent::Selection);
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_none());

        // Dismissing keeps the banner hidden even when typing again.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("again", window, cx);
                composer.on_prompt_change(cx);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_some());
        vcx.update(|_, cx| {
            cx.update_default_global::<branch_mismatch::DismissedBranchMismatches, _>(|set, _| {
                set.0.insert("t1:feat/a:main".into());
            });
            composer.update(cx, |composer, cx| composer.sync_branch_mismatch(cx));
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("branch-mismatch-banner").is_none());
    }

    #[gpui::test]
    fn pending_user_input_question_collapses_renders_markdown_toasts_displaced_text_and_gates_operate_scope(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _guard = runtime.enter();
        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (read_client, _read_fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read"]
            }))
            .unwrap(),
            "http://127.0.0.1:1".into(),
        );
        let (op_client, _op_fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            }))
            .unwrap(),
            "http://127.0.0.1:1".into(),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        let apply_question_snapshot = |state: &mut AppState, cx: &mut Context<AppState>| {
            let mut shell = shell.clone();
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t-q", "projectId": "checkout-a", "title": "Question Thread",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t-q"},
                "status": "running", "hasActionableProposedPlan": false, "itemCount": 1, "visibleItemCount": 1
            }))
            .unwrap()];
            state.shell = shell;
            state.select_thread("t-q", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": false,
                "projection": {
                    "thread": {
                        "id": "t-q", "projectId": "checkout-a", "title": "Question Thread",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t-q"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "running",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": "2026-04-01T00:00:01.000Z",
                        "completedAt": null
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [],
                    "runtimeRequests": [{
                        "id": "q-req-1",
                        "kind": "user_input",
                        "status": "pending",
                        "responseCapability": { "type": "live" },
                        "createdAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "messages": [], "plans": [],
                    "turnItems": [{
                        "id": "item-q1",
                        "threadId": "t-q",
                        "runId": "r1",
                        "nodeId": null,
                        "parentItemId": null,
                        "ordinal": 1,
                        "status": "running",
                        "title": null,
                        "startedAt": "2026-04-01T00:00:02.000Z",
                        "completedAt": null,
                        "updatedAt": "2026-04-01T00:00:02.000Z",
                        "type": "user_input_request",
                        "requestId": "q-req-1",
                        "providerRequestId": "q-req-1",
                        "statusDetail": "pending",
                        "questions": [{
                            "id": "q1",
                            "header": "Mode",
                            "question": "Pick **mode** with `Markdown`?",
                            "multiSelect": false,
                            "options": [{"label": "Fast", "value": "fast", "description": "Use fast mode"}]
                        }],
                        "answers": null
                    }],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::Selection);
        };

        state.update(cx, |state, cx| {
            state.install_active_environment(read_client, shell.clone(), state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            apply_question_snapshot(state, cx);
        });

        let (composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("question-toggle").is_some());
        assert!(vcx.debug_bounds("question-body").is_some());

        // Without orchestration:operate scope, digit selection is ignored and drafts stay untouched.
        composer.update(vcx, |composer, cx| {
            assert!(!composer.can_answer(cx));
        });
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                assert!(!composer.pick_by_digit(1, window, cx));
            });
        });

        // Switch to operate scope, verify collapse hides the body and disables digit shortcuts.
        state.update(vcx, |state, cx| {
            state.install_active_environment(op_client, shell.clone(), state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            apply_question_snapshot(state, cx);
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        composer.update(vcx, |composer, cx| {
            assert!(composer.can_answer(cx));
            composer.question_collapsed = Some("q1".to_owned());
            cx.notify();
        });
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("question-toggle").is_some());
        assert!(vcx.debug_bounds("question-body").is_none());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                assert!(!composer.pick_by_digit(1, window, cx));
                composer.question_collapsed = None;
                cx.notify();
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("question-body").is_some());

        // Typing custom text then picking an option displaces the text and emits the info toast.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("my typed answer", window, cx);
                composer.on_prompt_change(cx);
                assert!(composer.pick_by_digit(1, window, cx));
            });
        });
        let toast_title = state.read_with(vcx, |state, _| state.toasts.latest().map(|t| t.title.to_string()));
        assert_eq!(toast_title.as_deref(), Some("Your typed text was set aside"));
    }
}
