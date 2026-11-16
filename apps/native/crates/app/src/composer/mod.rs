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
mod rich_text;
mod send;
mod stash;
pub(crate) use pending::{banner_surface, banner_surface_card};
mod view;
mod workspace;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, Subscription, Task, Window, actions, div, prelude::*, px};
use gpui_component::h_flex;
use gpui_component::input::{EditorState, InputEvent, OutdentInline};
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

actions!(
    composer,
    [
        OutdentList,
        ToggleModelPicker,
        OpenPermissions,
        PasteAsText,
        StashPrompt,
        FocusPrompt,
        OpenWorkspace,
        OpenHost,
        CycleHost,
        UsePreviousWorktree,
        SendAlternate,
        SteerQueuedMessage,
        EditQueuedMessage,
    ]
);

pub fn init(cx: &mut App) {
    // `modelPicker.toggle`, `composer.effort` and `composer.mode` bind through the keymap.
    cx.bind_keys([
        KeyBinding::new("cmd-c", gpui_component::input::Copy, Some("ContextChip")),
        KeyBinding::new("shift-tab", OutdentList, Some("Composer > Input")),
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
    multi_models: Vec<ModelSelection>,
    runtime_mode: Option<RuntimeMode>,
    interaction_mode: Option<InteractionMode>,
}

/// `StopPhase` (`ComposerPrimaryActions.tsx`): Stop turns Pending -> TimedOut after 10s, and
/// transitions to a 500ms Landed click-guard window once the run stops being interruptible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum StopPhase {
    #[default]
    Idle,
    Pending,
    TimedOut,
    Landed,
}

const STOP_PENDING_MAX: Duration = Duration::from_secs(10);
const STOP_LANDED_CLICK_GUARD: Duration = Duration::from_millis(500);

/// What the composer reads from app state, kept so streamed events that change nothing it shows
/// do not repaint it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Snapshot {
    environment: String,
    target: Option<Target>,
    project_id: Option<String>,
    running: bool,
    active_run_id: Option<String>,
    can_interrupt: bool,
    can_operate_thread: bool,
    thread_model: Option<ModelSelection>,
    thread_runtime: Option<RuntimeMode>,
    thread_interaction: Option<InteractionMode>,
    /// The thread has run, so switching providers needs a handoff it may not support.
    started: bool,
    handoff: bool,
    /// True when the thread already has projected messages or visible items (`/clear` history gate).
    has_existing_history: bool,
    /// Active compaction blocks steering (`compactionBlocksSteering`), allowing auto-compaction
    /// steals only when the session advertises `activeSteeringDuringCompaction`.
    compaction_blocks_steer: bool,
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
    resumable_run_id: Option<String>,
    has_held_queued_runs: bool,
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
    multi_models: Vec<ModelSelection>,
    multi_models_enabled: bool,
    multi_model_label: Option<String>,
    multi_model_tooltip: Option<String>,
    model_label: String,
    driver: String,
    mark: Option<ProviderMark>,
    descriptors: Vec<OptionDescriptor>,
    /// The options a turn carries (`modelOptionsForDispatch`).
    dispatch_options: Vec<OptionSelection>,
    runtime_modes: Vec<RuntimeMode>,
    runtime_mode: RuntimeMode,
    default_runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    locked_instance: Option<String>,
}

#[derive(Debug, Clone)]
struct DisplacedAnswer {
    draft: String,
    key: String,
    text: String,
}

#[derive(Debug, Clone, Default)]
struct Answers {
    thread_id: String,
    drafts: HashMap<String, DraftAnswer>,
    index: usize,
}

struct PromptSuggestionWatcher {
    thread_id: String,
    environment: String,
    connection_id: Option<u64>,
    generation: Option<u64>,
    suggestion: Option<rules::PromptSuggestion>,
    _net: tokio::task::JoinHandle<()>,
    _ui: Task<()>,
}

impl Drop for PromptSuggestionWatcher {
    fn drop(&mut self) {
        self._net.abort();
    }
}

pub struct Composer {
    state: Entity<AppState>,
    input: Entity<EditorState>,
    rich_text: rich_text::RichTextState,
    picker: Entity<ModelPicker>,
    picker_open: bool,
    permissions_open: bool,
    choices: HashMap<Target, Choices>,
    /// The model and mode a new draft starts from: what the thread before it ran.
    carry: (Option<ModelSelection>, Option<InteractionMode>),
    snapshot: Snapshot,
    pub(crate) provider_target: ProviderTarget,
    provider_choice: Option<String>,
    responding: HashSet<String>,
    answers: HashMap<String, Answers>,
    displaced_answers: Vec<DisplacedAnswer>,
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
    prompt_length_error: Option<String>,
    auto_advance: Option<Task<()>>,
    /// Whether the last Enter key-down was an OS key repeat. Recorded by the key listener, which
    /// runs before the textarea's `PressEnter` event reaches the composer.
    enter_repeat: bool,
    picker_dismissed: Option<Instant>,
    menu_dismissed: Option<Instant>,
    input_state: Option<(String, bool, bool)>,
    /// The question banner takes focus for its digit shortcuts while the prompt cannot type.
    question_focus: FocusHandle,
    limit_recovery: Entity<crate::thread_view::LimitRecoveryBanner>,
    usage_limits: Option<UsageLimitsPanel>,
    checkout_vcs: Option<branch_mismatch::CheckoutWatcher>,
    revealed_branch_mismatch_key: Option<String>,
    restoring_branch_threads: HashSet<String>,
    fresh_session_arms: HashMap<(String, Target), u64>,
    fresh_session_seq: u64,
    keep_full_history: HashSet<(String, Target)>,
    compact_expiry_timer: Option<Task<()>>,
    stop_phase: StopPhase,
    stop_run_id: Option<String>,
    stop_timer_seq: u64,
    stop_timer: Option<Task<()>>,
    prompt_suggestion: Option<PromptSuggestionWatcher>,
    dismissed_prompt_suggestions: HashSet<(String, String)>,
    /// A side chat's own thread, in the right panel; `None` follows the selection and draft.
    pub(super) fixed: Option<String>,
    /// The `@`, `/` and `$` menus.
    menu: Menu,
    /// Staged images and files per target.
    attachments: attachments::Attachments,
    /// Terminal excerpts per target and the chip row's popover.
    context: context::ContextChips,
    citation_editor: Option<citations::CommentEditor>,
    pub(crate) voice: Entity<crate::voice::VoiceSpeechController>,
    voice_pending_intent: Option<rules::SubmitIntent>,
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

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn snapshot_voice(&self) -> Entity<crate::voice::VoiceSpeechController> {
        self.voice.clone()
    }
}

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

pub(crate) fn thread_choices(thread_id: &str, cx: &App) -> (Option<ModelSelection>, Option<RuntimeMode>) {
    let Some(reg) = cx.try_global::<RegisteredComposers>() else {
        return (None, None);
    };
    let target = Target::Thread(thread_id.to_owned());
    let composer = reg
        .0
        .iter()
        .filter_map(|weak| weak.upgrade())
        .find(|entity| {
            let c = entity.read(cx);
            c.fixed.is_none() && c.thread_id().as_deref() == Some(thread_id)
        })
        .or_else(|| {
            reg.0
                .iter()
                .filter_map(|weak| weak.upgrade())
                .find(|entity| entity.read(cx).thread_id().as_deref() == Some(thread_id))
        });
    let Some(entity) = composer else {
        return (None, None);
    };
    let choices = entity.read(cx).choices.get(&target);
    (choices.and_then(|c| c.model.clone()), choices.and_then(|c| c.runtime_mode))
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
            EditorState::new(window, cx)
                .searchable(false)
                .line_number(false)
                .indent_guides(false)
                .folding(false)
                .soft_wrap(true)
                .scroll_beyond_last_line(Some(0))
                .placeholder(rules_placeholder())
                .submit_on_enter(true)
        });
        let decorations = input.update(cx, |input, cx| input.create_decorations_collection(Vec::new(), cx));
        let picker = cx.new(|cx| ModelPicker::new(state.clone(), window, cx));
        let setup = fixed.is_none().then(|| crate::worktree_setup::store(&state, cx));
        let composer_weak = cx.entity().downgrade();
        let composer_weak_hints = composer_weak.clone();
        let composer_weak_proj = composer_weak.clone();
        let composer_weak_env = composer_weak.clone();
        let composer_weak_focus = composer_weak.clone();
        let composer_weak_disabled = composer_weak.clone();
        let voice = cx.new(|cx| {
            crate::voice::VoiceSpeechController::new(
                crate::voice::VoiceSpeechConfig {
                    app: state.clone(),
                    owner_key: fixed
                        .as_ref()
                        .map(|id| format!("thread:{id}"))
                        .unwrap_or_else(|| "composer:initial".to_owned()),
                    offer_setup: true,
                    draft_reader: std::rc::Rc::new(move |cx| {
                        composer_weak.upgrade().map(|c| c.read(cx).voice_draft_snapshot(cx))
                    }),
                    hints_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_hints
                            .upgrade()
                            .map(|c| c.read(cx).voice_vocabulary_hints(cx))
                            .unwrap_or_default()
                    }),
                    project_id_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_proj
                            .upgrade()
                            .and_then(|c| c.read(cx).snapshot.project_id.clone())
                    }),
                    environment_id_reader: std::rc::Rc::new(move |cx| {
                        composer_weak_env
                            .upgrade()
                            .map(|c| c.read(cx).environment_id(cx))
                            .filter(|id| !id.is_empty())
                    }),
                    on_open_voice_settings: std::rc::Rc::new(|_, cx| {
                        crate::settings::open_voice(cx);
                    }),
                },
                cx,
            )
        });
        let _dictation_target_id = crate::voice::register_dictation_target(
            &voice,
            false,
            std::rc::Rc::new(move |window, cx| {
                composer_weak_focus
                    .upgrade()
                    .is_some_and(|c| c.read(cx).input.read(cx).focus_handle(cx).is_focused(window))
            }),
            std::rc::Rc::new(move |cx| {
                composer_weak_disabled
                    .upgrade()
                    .is_none_or(|c| c.read(cx).voice_is_disabled(cx))
            }),
            cx,
        );
        let mut subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { secondary, shift } => this.on_enter(*secondary, *shift, window, cx),
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
                    this.retry_offline_staged_uploads(cx);
                }
                if *event == AppEvent::AttachmentUpload {
                    this.apply_upload_events(cx);
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
                if *event == AppEvent::ComposerReply
                    && let Some(thread_id) = this.thread_id()
                    && this.state.read(cx).composer_reply.as_ref().is_some_and(|(id, _)| *id == thread_id)
                    && let Some((_, reply)) = this.state.update(cx, |state, _| state.composer_reply.take())
                    && !this.reply_to_response(reply, window, cx)
                {
                    this.state.update(cx, |state, cx| {
                        state.show_toast(
                            crate::toast::Toast::new(crate::toast::ToastKind::Warning, "The composer is not ready")
                                .description("Try replying after the connection or pending input is resolved.".to_owned()),
                            cx,
                        );
                    });
                }
                if *event == AppEvent::ComposerRestore {
                    this.restore_prompt(window, cx);
                }
                if *event == AppEvent::Rewind {
                    if this.is_rewinding(cx) {
                        let _ = this.cancel_pending_send();
                        this.picker_open = false;
                        this.permissions_open = false;
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
            cx.observe(&input, |this, _, cx| {
                this.sync_menu(false, cx);
                this.sync_rich_decorations(cx);
            }),
            cx.subscribe_in(&voice, window, |this, _, event: &crate::voice::VoiceControllerEvent, window, cx| {
                match event {
                    crate::voice::VoiceControllerEvent::StateChanged => {
                        this.sync_input_state(window, cx);
                        cx.notify();
                    }
                    crate::voice::VoiceControllerEvent::CommitDraft { text, selection } => {
                        let caret = t3_core::voice_input::utf16_to_utf8_offset(text, selection.start)
                            .min(text.len());
                        this.input.update(cx, |input, cx| {
                            let current_len = input.value().len();
                            input.set_selected_range(0..current_len, cx);
                            input.replace(text.clone(), window, cx);
                            input.set_selected_range(caret..caret, cx);
                            input.focus(window, cx);
                        });
                        this.on_prompt_change(cx);
                        cx.notify();
                    }
                    crate::voice::VoiceControllerEvent::SubmitAfterDictation { .. } => {
                        let intent = this
                            .voice_pending_intent
                            .take()
                            .unwrap_or(rules::SubmitIntent::Foreground);
                        this.submit(intent, window, cx);
                    }
                }
            }),
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
            rich_text: rich_text::RichTextState::new(decorations),
            picker,
            picker_open: false,
            permissions_open: false,
            choices: HashMap::new(),
            carry: (None, None),
            snapshot: Snapshot::default(),
            provider_target: ProviderTarget::default(),
            provider_choice: None,
            responding: HashSet::new(),
            answers: HashMap::new(),
            displaced_answers: Vec::new(),
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
            prompt_length_error: None,
            auto_advance: None,
            enter_repeat: false,
            picker_dismissed: None,
            menu_dismissed: None,
            input_state: None,
            question_focus: cx.focus_handle(),
            fresh_session_arms: HashMap::new(),
            fresh_session_seq: 0,
            keep_full_history: HashSet::new(),
            compact_expiry_timer: None,
            stop_phase: StopPhase::Idle,
            stop_run_id: None,
            stop_timer_seq: 0,
            stop_timer: None,
            prompt_suggestion: None,
            dismissed_prompt_suggestions: HashSet::new(),
            fixed,
            menu: Menu::default(),
            attachments: Default::default(),
            context: Default::default(),
            citation_editor: None,
            voice,
            voice_pending_intent: None,
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
        let can_operate_thread = state.client_handle().is_none_or(|client| {
            client
                .session_state()
                .borrow()
                .as_ref()
                .is_none_or(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
        });
        let Some(Target::Thread(thread_id)) = &target else {
            let project_id = state.draft_project.clone();
            return Snapshot { environment, target, project_id, can_operate_thread, last_error, ..Snapshot::default() };
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
        let active_run_id = projection.and_then(workflows::active_run).map(|run| run.id.clone());
        let can_interrupt = running || projection.is_some_and(workflows::has_pending_background_work);
        let setup = self.setup.as_ref().and_then(|setup| setup.read(cx).view_for(Some(thread_id)));
        let (setup_blocks_send, setup_queues) = setup.map_or((false, false), |view| (view.blocks_send, view.queues_behind_setup));
        let resumable_run_id = projection.and_then(t3_core::timeline::plan::resumable_run_id);
        let has_held_queued_runs = projection.is_some_and(|p| p.runs.iter().any(|run| run.status == "queued" && run.queue_held == Some(true)));
        Snapshot {
            environment,
            setup_blocks_send,
            setup_queues,
            project_id: shell.map(|thread| thread.project_id.clone()),
            running,
            active_run_id: active_run_id.clone(),
            can_interrupt,
            can_operate_thread,
            thread_model: shell.map(|thread| thread.model_selection.clone()),
            thread_runtime: shell.and_then(|thread| RuntimeMode::parse(&thread.runtime_mode)),
            thread_interaction: shell.and_then(|thread| InteractionMode::parse(&thread.interaction_mode)),
            started: projection.is_some_and(|projection| !projection.runs.is_empty()),
            handoff: projection.is_some_and(workflows::supports_provider_handoff),
            has_existing_history: projection.is_some_and(|p| !p.messages.is_empty() || !p.visible_turn_items.is_empty()),
            compaction_blocks_steer: running
                && projection.is_some_and(|projection| workflows::compaction_blocks_steering(projection, active_run_id.as_deref())),
            loaded: projection.is_some(),
            pending_key,
            pending,
            queue: projection.map(t3_core::composer::queue::queue_state).unwrap_or_default(),
            resumable_run_id,
            has_held_queued_runs,
            can_resume: projection.is_some_and(t3_core::timeline::plan::can_resume_thread),
            last_error,
            target,
        }
    }

    /// Re-reads app state. Notifies only when something the composer shows changed.
    fn refresh(&mut self, config_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.watch_question_grants(cx);
        let next = self.read_snapshot(cx);
        let switched = next.target != self.snapshot.target || next.environment != self.snapshot.environment;
        if switched {
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
            self.prompt_length_error = None;
            self.auto_advance = None;
            if matches!(next.target, Some(Target::Draft(_)))
                || (!cfg!(test) && self.fixed.is_none() && matches!(next.target, Some(Target::Thread(_))))
            {
                self.focus(window, cx);
            }
        }
        // Return unsent answers + referenced thread chips from cancelled/expired requests BEFORE pruning.
        let returned_answers = self.return_unanswered_user_inputs(&next.environment, &next.target, window, cx);
        let pruned = self.prune_question_attachments(&next.environment, cx);
        if self.pending_send.is_pending() && self.question_revision(cx) != self.pending_questions && self.cancel_pending_send() { cx.notify(); }
        if next.target != self.snapshot.target || next.pending.user_inputs != self.snapshot.pending.user_inputs {
            self.cancel_pending_send();
        }
        if next == self.snapshot && !config_changed {
            if self.provider_target.connection != self.state.read(cx).connection_id() { self.sync_provider_target(cx); }
            self.sync_checkout_vcs(cx);
            self.sync_branch_mismatch(cx);
            self.sync_prompt_suggestion(cx);
            if self.finish_prepared_reads(cx) || pruned || returned_answers { cx.notify(); }
            return;
        }
        // `pruneResolvedUserInputDrafts`: prune any loaded thread on this environment; unloaded threads wait.
        let pending_ids: Vec<&String> = next.pending.user_inputs.iter().map(|input| &input.request_id).collect();
        self.answers.retain(|key, answers| {
            let Ok(parts) = serde_json::from_str::<Vec<String>>(key) else { return true };
            if parts.first() != Some(&next.environment) {
                return true;
            }
            let thread_id = &answers.thread_id;
            if next.target.as_ref() == Some(&Target::Thread(thread_id.clone())) && next.loaded {
                return pending_ids.iter().any(|id| key == &Self::answer_key_for(&next.environment, thread_id, id));
            }
            let Some(projection) = self.state.read(cx).thread_state(thread_id).projection.as_ref().filter(|p| p.thread.id == *thread_id) else {
                return true;
            };
            projection
                .runtime_requests
                .iter()
                .filter(|req| req.value.get("status").and_then(serde_json::Value::as_str) == Some("pending"))
                .any(|req| key == &Self::answer_key_for(&next.environment, thread_id, &req.id))
        });
        self.responding.retain(|request_id| {
            if let Ok(parts) = serde_json::from_str::<Vec<String>>(request_id) {
                return parts.len() == 3 && (!next.loaded || parts[0] != next.environment || Some(&parts[1]) != next.target.as_ref().and_then(|target| if let Target::Thread(id) = target { Some(id) } else { None }) || pending_ids.iter().any(|id| **id == parts[2]));
            }
            pending_ids.contains(&request_id)
        });
        self.snapshot = next;
        let voice_owner = match &self.snapshot.target {
            Some(Target::Thread(id)) => format!("{}:thread:{id}", self.snapshot.environment),
            Some(Target::Draft(id)) => format!("{}:draft:{id}", self.snapshot.environment),
            Some(Target::Question(key)) => {
                format!("{}:question:{}:{}", self.snapshot.environment, key.request, key.question)
            }
            None => self.snapshot.environment.clone(),
        };
        self.voice.update(cx, |voice, cx| voice.set_owner_key(voice_owner, cx));
        self.sync_stop_phase(cx);
        self.sync_compact_expiry_timer(cx);
        self.sync_provider_target(cx);
        self.sync_usage_limits(cx);
        self.sync_checkout_vcs(cx);
        self.sync_branch_mismatch(cx);
        self.sync_prompt_suggestion(cx);
        if config_changed {
            self.reconcile_upload_capabilities(cx);
        }
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
            .clone()
            .or_else(|| self.snapshot.thread_model.clone())
            .or_else(|| draft.as_ref().and_then(|draft| draft.model_selection.clone()));
        let entries = config.map(|config| providers::entries(config, self.target_project())).unwrap_or_default();
        let model = match config {
            Some(_) => rules::effective_selection(&entries, chosen.as_ref()),
            None => chosen,
        };
        let cwd = self.menu_cwd(cx);
        let multi_models_enabled = matches!(self.snapshot.target, Some(Target::Draft(_)))
            && self.fixed.is_none()
            && config.is_some_and(|config| config.environment.capabilities.required_worktree_bootstrap == Some(true));
        let multi_models = if multi_models_enabled && choices.multi_models.len() > 1 {
            choices.multi_models.clone()
        } else {
            Vec::new()
        };
        let resolve_model_entry = |selection: &ModelSelection| {
            let provider = config?.provider(&selection.instance_id)?;
            let snapshot = self.workspace_snapshot_for(provider, cwd.as_deref());
            providers::models_for_cwd(provider, snapshot).iter().find(|m| m.slug == selection.model)
        };
        let (multi_model_label, multi_model_tooltip) = if !multi_models.is_empty() {
            let label = providers::format_multiple_model_label(&multi_models, |selection| {
                resolve_model_entry(selection)
                    .map(|m| providers::trigger_model_name(m).to_owned())
                    .unwrap_or_else(|| selection.model.clone())
            });
            let tooltip = multi_models
                .iter()
                .map(|selection| {
                    resolve_model_entry(selection)
                        .map(|m| providers::model_name(m).to_owned())
                        .unwrap_or_else(|| selection.model.clone())
                })
                .collect::<Vec<_>>()
                .join(", ");
            (Some(label), Some(tooltip))
        } else {
            (None, None)
        };
        let mark_model = multi_models.first().or(model.as_ref());
        let mark = mark_model
            .and_then(|model| entries.iter().find(|entry| entry.instance_id() == model.instance_id))
            .map(|entry| ProviderMark::of(entry, &entries));
        let provider = model.as_ref().and_then(|model| config?.provider(&model.instance_id));
        let snapshot = provider.and_then(|provider| self.workspace_snapshot_for(provider, cwd.as_deref()));
        let provider_model = model
            .as_ref()
            .and_then(|selection| providers::models_for_cwd(provider?, snapshot).iter().find(|m| m.slug == selection.model));
        let runtime = choices
            .runtime_mode
            .or(self.snapshot.thread_runtime)
            .or_else(|| draft.as_ref().map(|draft| draft.runtime_mode))
            .unwrap_or_default();
        let (runtime_modes, runtime_mode) = rules::compatible_runtime_modes(runtime, provider);
        let driver = provider.map(|p| p.driver.clone()).unwrap_or_default();
        let explicit = traits::selections(model.as_ref().and_then(|m| m.options.as_ref()));
        let options = traits::composer_options(&driver, provider_model, false, &explicit);
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
            multi_models,
            multi_models_enabled,
            multi_model_label,
            multi_model_tooltip,
            descriptors: options.descriptors,
            dispatch_options: options.dispatch,
            runtime_modes,
            runtime_mode,
            default_runtime_mode,
            interaction_mode: InteractionMode::Default,
            locked_instance,
        }
    }

    pub(crate) fn resolved_provider_instance(&self, cx: &App) -> Option<String> {
        self.resolve(cx).model.map(|model| model.instance_id)
    }

    pub(super) fn has_multiple_models(&self, cx: &App) -> bool {
        !self.resolve(cx).multi_models.is_empty()
    }

    fn picker_input(&self, cx: &App) -> PickerInput {
        let resolved = self.resolve(cx);
        let cwd = self.menu_cwd(cx);
        PickerInput {
            project_id: self.snapshot.project_id.clone(),
            cwd: cwd.clone(),
            workspace_snapshots: self.workspace_snapshots_for_cwd(cwd.as_deref()),
            workspace_scanning: self.workspace_scanning_for_cwd(cwd.as_deref()),
            selection: resolved.model,
            multi_models_enabled: resolved.multi_models_enabled,
            selected_models: resolved.multi_models,
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

    fn persist_thread_model_pick(&mut self, thread_id: &str, selection: &ModelSelection, cx: &mut Context<Self>) {
        if self.snapshot.thread_model.as_ref().is_none_or(|current| current.instance_id != selection.instance_id) {
            return;
        }
        let thread_id = thread_id.to_owned();
        let selection = selection.clone();
        let target = Target::Thread(thread_id.clone());
        let command = t3_core::commands::set_model_selection(&crate::app_state::uuid_v4(), &thread_id, &selection);
        let dispatch = self.state.read(cx).dispatch_in_order(vec![command]);
        cx.spawn(async move |this, cx| {
            let result = dispatch.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    if let Some(choices) = this.choices.get_mut(&target)
                        && choices.model.as_ref() == Some(&selection)
                    {
                        choices.model = None;
                        this.draft_changed(cx);
                        cx.notify();
                    }
                }
                Err((_, error)) => {
                    let toast = Toast::new(ToastKind::Error, "Could not apply model selection")
                        .description(format!("This model pick was not saved. {error}"));
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                }
            });
        })
        .detach();
    }

    fn on_picker(&mut self, event: &PickerEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            self.picker_open = false;
            return;
        }
        match event {
            PickerEvent::Model { instance_id, model } => {
                let resolved = self.resolve(cx);
                let same_shown = resolved.multi_models.is_empty()
                    && resolved
                        .model
                        .as_ref()
                        .is_some_and(|current| current.instance_id == *instance_id && current.model == *model);
                let selection = if same_shown {
                    resolved.model.unwrap_or_else(|| ModelSelection { instance_id: instance_id.clone(), model: model.clone(), options: None })
                } else {
                    ModelSelection { instance_id: instance_id.clone(), model: model.clone(), options: None }
                };
                if !same_shown && let Some(choices) = self.choices_mut() {
                    choices.multi_models.clear();
                    choices.model = Some(selection.clone());
                }
                if !same_shown {
                    self.draft_changed(cx);
                    self.sync_usage_limits(cx);
                }
                if let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() {
                    self.persist_thread_model_pick(&thread_id, &selection, cx);
                }
            }
            PickerEvent::ToggleModel { instance_id, model } => {
                let resolved = self.resolve(cx);
                if !resolved.multi_models_enabled {
                    return;
                }
                let mut current = if !resolved.multi_models.is_empty() {
                    resolved.multi_models
                } else {
                    resolved.model.into_iter().collect()
                };
                if let Some(pos) = current.iter().position(|item| item.instance_id == *instance_id && item.model == *model) {
                    current.remove(pos);
                } else {
                    current.push(ModelSelection { instance_id: instance_id.clone(), model: model.clone(), options: None });
                }
                if let Some(choices) = self.choices_mut() {
                    if current.len() > 1 {
                        choices.multi_models = current;
                    } else {
                        choices.multi_models.clear();
                        if let Some(remaining) = current.into_iter().next() {
                            choices.model = Some(remaining);
                        }
                    }
                }
                self.draft_changed(cx);
                self.sync_usage_limits(cx);
            }
            PickerEvent::InspectInstance(instance_id) => {
                self.ensure_workspace_snapshot(instance_id, cx);
            }
            PickerEvent::Options(selections) => {
                self.set_options(selections, cx);
                self.draft_changed(cx);
                if let Some(Target::Thread(thread_id)) = self.snapshot.target.clone()
                    && let Some(selection) = self.resolve(cx).model
                {
                    self.persist_thread_model_pick(&thread_id, &selection, cx);
                }
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

    /// Shift+Tab: outdent a fenced code block or list item under the caret, else dispatch `OutdentInline`.
    fn outdent_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let (value, range) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range())
        };
        if let Some(edit) = rules::rich_text::code_block_indent(&value, range.start, range.end, true) {
            self.apply_rich_edit(edit, window, cx);
            return;
        }
        if let Some(edit) = lists::outdent_for_tab(&value, range.start, range.end) {
            self.input.update(cx, |input, cx| {
                input.set_selected_range(edit.start..edit.end, cx);
                input.replace(edit.replacement, window, cx);
                input.set_selected_range(edit.cursor_after..edit.cursor_after, cx);
            });
            return;
        }
        window.dispatch_action(Box::new(OutdentInline), cx);
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

    pub(crate) fn set_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.cancel_pending_send() { cx.notify(); }
        if text.is_empty() {
            self.prompt_length_error = None;
        }
        self.close_citation_comment("prompt-replaced", window, cx);
        self.has_text = !text.trim().is_empty();
        self.input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
    }

    pub(crate) fn replace_prompt_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_prompt(text, window, cx);
        self.draft_changed(cx);
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

    pub(super) fn voice_draft_snapshot(&self, cx: &App) -> t3_core::voice_input::VoiceDraftSnapshot {
        let input = self.input.read(cx);
        let text = input.value().to_string();
        let range = input.selected_range();
        t3_core::voice_input::VoiceDraftSnapshot {
            owner_key: self.voice.read(cx).owner_key().to_owned(),
            selection: t3_core::voice_input::VoiceSelection {
                start: t3_core::voice_input::utf8_to_utf16_offset(&text, range.start),
                end: t3_core::voice_input::utf8_to_utf16_offset(&text, range.end),
            },
            text,
        }
    }

    pub(super) fn voice_vocabulary_hints(&self, cx: &App) -> Vec<String> {
        let Some(project_id) = self.snapshot.project_id.as_deref() else {
            return Vec::new();
        };
        self.state
            .read(cx)
            .shell
            .snapshot
            .as_ref()
            .and_then(|snap| snap.projects.iter().find(|p| p.id == project_id))
            .map(|p| vec![p.title.clone()])
            .unwrap_or_default()
    }

    pub(super) fn voice_is_disabled(&self, cx: &App) -> bool {
        self.sending
            || self.is_rewinding(cx)
            || !self.snapshot.can_operate_thread
            || self.snapshot.setup_blocks_send
            || self.active_input().is_some_and(|input| {
                !self.can_answer(cx) || !input.can_respond() || self.question_responding(&input.request_id, cx)
            })
            || self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer())
    }

    fn on_enter(&mut self, modifier: bool, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.open && self.workspace_enabled() {return;}
        let composing = self.input.update(cx, |input, cx| input.marked_text_range(window, cx).is_some());
        let key = EnterKey { modifier, repeat: std::mem::take(&mut self.enter_repeat), composing };
        let (prompt, sel) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.selected_range())
        };
        if !composing
            && !modifier
            && self.answering.is_none()
            && let Some(edit) = rules::rich_text::code_block_enter(&prompt, sel.start, sel.end)
        {
            self.apply_rich_edit(edit, window, cx);
            return;
        }
        if shift {
            if !composing {
                self.input.update(cx, |input, cx| input.insert("\n", window, cx));
            }
            return;
        }
        let settings = self.state.read(cx).client_settings;
        let draft = matches!(self.snapshot.target, Some(Target::Draft(_)));
        match rules::submit_intent(key, self.snapshot.running, draft, settings.send_shortcut, &prompt) {
            Some(intent) => {
                if self.voice.read(cx).blocks_submission() {
                    if !self.voice.read(cx).preparing() {
                        self.voice_pending_intent = Some(intent);
                        self.voice.update(cx, |voice, cx| {
                            voice.submit_after_dictation(cx);
                        });
                    }
                } else {
                    self.submit(intent, window, cx);
                }
            }
            // The input method commits its composition on Enter.
            None if composing => {}
            None => {
                if !modifier
                    && self.answering.is_none()
                    && sel.start == sel.end
                    && let Some(edit) = lists::continuation_for_enter(&prompt, sel.start)
                {
                    self.input.update(cx, |input, cx| {
                        input.set_selected_range(edit.start..edit.end, cx);
                        input.replace(edit.replacement, window, cx);
                        input.set_selected_range(edit.cursor_after..edit.cursor_after, cx);
                    });
                } else {
                    self.input.update(cx, |input, cx| input.insert("\n", window, cx));
                }
            }
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
        let mut validation_changed = false;
        if self.prompt_length_error.is_some() {
            let outgoing = self.outgoing(text.clone(), &self.staged_attachments());
            let next_error = rules::prompt_length_validation_message(&text, Some(&outgoing.text));
            if next_error != self.prompt_length_error {
                self.prompt_length_error = next_error;
                validation_changed = true;
            }
        }
        self.sync_citation_comment(&text, cx);
        self.sync_context_chips(&text, cx);
        self.sync_branch_mismatch(cx);
        let has_text = !text.trim().is_empty();
        let answering = self.active_question();
        if let Some((request_id, question)) = &answering {
            self.withdraw_restored_answer(request_id, &question.id, &text, cx);
            let thread_id = self.thread_id().unwrap_or_default();
            let answer_key = self.answer_key(request_id);
            let answers = self.answers.entry(answer_key).or_insert_with(|| Answers { thread_id, ..Answers::default() });
            let next = requests::set_custom_answer(question, answers.drafts.get(&question.id), &text);
            answers.drafts.insert(question.id.clone(), next);
        }
        if answering.is_some() || has_text != self.has_text || validation_changed {
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

    pub(super) fn fresh_session_available(&self, cx: &App) -> bool {
        matches!(self.snapshot.target, Some(Target::Thread(_)))
            && self.snapshot.has_existing_history
            && self
                .state
                .read(cx)
                .server_config
                .config
                .as_ref()
                .is_some_and(|config| config.environment.capabilities.fresh_context == Some(true))
    }

    pub(super) fn fresh_session_command_available(&self, cx: &App) -> bool {
        self.fresh_session_available(cx)
            && self.snapshot.pending.approvals.is_empty()
            && self.snapshot.pending.user_inputs.is_empty()
    }

    fn fresh_session_arm(&self, cx: &App) -> Option<(String, Target, u64)> {
        if !self.fresh_session_command_available(cx) {
            return None;
        }
        let target = self.snapshot.target.clone()?;
        let key = (self.snapshot.environment.clone(), target.clone());
        let seq = *self.fresh_session_arms.get(&key)?;
        Some((key.0, target, seq))
    }

    pub(super) fn is_fresh_session_armed(&self, cx: &App) -> bool {
        self.fresh_session_arm(cx).is_some()
    }

    pub(super) fn arm_fresh_session(&mut self, cx: &mut Context<Self>) {
        if !self.fresh_session_command_available(cx) {
            return;
        }
        let Some(target) = self.snapshot.target.clone() else { return };
        self.fresh_session_seq = self.fresh_session_seq.wrapping_add(1);
        self.fresh_session_arms.insert((self.snapshot.environment.clone(), target), self.fresh_session_seq);
        cx.notify();
    }

    pub(super) fn disarm_fresh_session(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        if self.fresh_session_arms.remove(&(self.snapshot.environment.clone(), target)).is_some() {
            cx.notify();
        }
    }

    fn consume_fresh_session_arm(&mut self, arm: (String, Target, u64), cx: &mut Context<Self>) {
        let (env, target, expected_seq) = arm;
        let key = (env, target);
        if self.fresh_session_arms.get(&key).copied() == Some(expected_seq) {
            self.fresh_session_arms.remove(&key);
            cx.notify();
        }
    }

    fn compact_before_send_tokens(&self, resolved: &Resolved, cx: &App) -> Option<f64> {
        if self.snapshot.running || !resolved.multi_models.is_empty() {
            return None;
        }
        let Target::Thread(thread_id) = self.snapshot.target.as_ref()? else { return None };
        let selection = resolved.model.as_ref()?;
        let state = self.state.read(cx);
        let config = state.server_config.config.as_ref()?;
        let projection = state.thread_state(thread_id).projection.as_ref().filter(|p| p.thread.id == *thread_id)?;
        let prompt = self.prompt(cx);
        t3_core::usage::context_window::compact_before_send_tokens(
            projection,
            selection,
            &config.providers,
            &config.settings,
            &prompt,
            !self.snapshot.running,
            crate::sidebar::controller::now_ms(),
        )
    }

    pub(super) fn is_keep_full_history(&self, cx: &App) -> bool {
        let _ = cx;
        let Some(target) = self.snapshot.target.as_ref() else { return false };
        self.keep_full_history.contains(&(self.snapshot.environment.clone(), target.clone()))
    }

    pub(super) fn toggle_keep_full_history(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let key = (self.snapshot.environment.clone(), target);
        if !self.keep_full_history.remove(&key) {
            self.keep_full_history.insert(key);
        }
        cx.notify();
    }

    fn clear_keep_full_history(&mut self, environment: Option<&str>, target: &Target) {
        let env = environment.unwrap_or(&self.snapshot.environment).to_owned();
        self.keep_full_history.remove(&(env, target.clone()));
    }

    fn sync_compact_expiry_timer(&mut self, cx: &mut Context<Self>) {
        self.compact_expiry_timer = None;
        if self.snapshot.running {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.as_ref() else { return };
        let resolved = self.resolve(cx);
        if !resolved.multi_models.is_empty() {
            return;
        }
        let Some(selection) = resolved.model.as_ref() else { return };
        let state = self.state.read(cx);
        let Some(config) = state.server_config.config.as_ref() else { return };
        let Some(provider) = config.providers.iter().find(|p| p.instance_id == selection.instance_id) else { return };
        if provider.driver != "claudeAgent" {
            return;
        }
        let Some(projection) = state.thread_state(thread_id).projection.as_ref().filter(|p| p.thread.id == *thread_id) else {
            return;
        };
        let Some(window_snap) = t3_core::usage::context_window::context_window_snapshot(projection) else { return };
        if window_snap.used_tokens < t3_core::usage::context_window::COMPACTION_SUGGESTION_MIN_TOKENS {
            return;
        }
        let Some(updated_ms) = t3_core::sidebar::time::parse_iso(&window_snap.updated_at) else { return };
        let ttl_ms = window_snap
            .prompt_cache_ttl_ms
            .filter(|ttl| ttl.is_finite() && *ttl >= 0.0)
            .map(|ttl| ttl as i64)
            .unwrap_or(t3_core::usage::context_window::CLAUDE_RESUME_COMPACTION_FALLBACK_MS);
        let remaining_ms = updated_ms.saturating_add(ttl_ms).saturating_sub(crate::sidebar::controller::now_ms());
        if remaining_ms > 0 {
            self.compact_expiry_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(remaining_ms as u64 + 5)).await;
                let _ = this.update(cx, |this, cx| {
                    this.compact_expiry_timer = None;
                    cx.notify();
                });
            }));
        }
    }

    fn set_stop_phase(&mut self, next: StopPhase, cx: &mut Context<Self>) {
        if self.stop_phase == next {
            return;
        }
        self.stop_phase = next;
        self.stop_timer_seq = self.stop_timer_seq.wrapping_add(1);
        let seq = self.stop_timer_seq;
        self.stop_timer = match next {
            StopPhase::Pending => Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(STOP_PENDING_MAX).await;
                let _ = this.update(cx, |this, cx| {
                    if this.stop_timer_seq == seq && this.stop_phase == StopPhase::Pending {
                        this.stop_phase = StopPhase::TimedOut;
                        this.stop_timer = None;
                        cx.notify();
                    }
                });
            })),
            StopPhase::Landed => Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(STOP_LANDED_CLICK_GUARD).await;
                let _ = this.update(cx, |this, cx| {
                    if this.stop_timer_seq == seq && this.stop_phase == StopPhase::Landed {
                        this.stop_phase = StopPhase::Idle;
                        this.stop_timer = None;
                        cx.notify();
                    }
                });
            })),
            StopPhase::Idle | StopPhase::TimedOut => None,
        };
    }

    fn sync_stop_phase(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.active_run_id.is_some() && self.stop_run_id != self.snapshot.active_run_id {
            self.stop_run_id = self.snapshot.active_run_id.clone();
            self.set_stop_phase(StopPhase::Idle, cx);
            return;
        }
        if !self.snapshot.can_operate_thread {
            return;
        }
        if matches!(self.stop_phase, StopPhase::Pending | StopPhase::TimedOut) && !self.snapshot.can_interrupt {
            self.set_stop_phase(StopPhase::Landed, cx);
        } else if self.stop_phase == StopPhase::Landed && self.snapshot.can_interrupt {
            self.set_stop_phase(StopPhase::Idle, cx);
        }
    }

    pub(super) fn request_stop(&mut self, cx: &mut Context<Self>) {
        if !self.snapshot.can_operate_thread || self.stop_phase == StopPhase::Pending || self.is_rewinding(cx) {
            return;
        }
        self.stop_run_id = self.snapshot.active_run_id.clone();
        self.set_stop_phase(StopPhase::Pending, cx);
        let fixed = self.fixed.clone();
        self.state.update(cx, |state, cx| match fixed {
            Some(id) => state.stop_thread(&id, cx),
            None => state.stop_selected_thread(cx),
        });
        cx.notify();
    }

    pub(super) fn render_fresh_session_banner(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.is_fresh_session_armed(cx) || self.editing_queued().is_some() {
            return None;
        }
        let p = palette(cx);
        let dismiss = div()
            .id("fresh-session-dismiss")
            .px(px(6.))
            .h(px(22.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_xs()
            .text_color(hsla(p.muted_foreground))
            .cursor_pointer()
            .hover(|this| this.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
            .child("Keep the current session")
            .child(Icon::X.render(px(12.), hsla(p.muted_foreground)))
            .on_click(cx.listener(|this, _, _, cx| this.disarm_fresh_session(cx)));
        let content = h_flex()
            .w_full()
            .min_h(px(24.))
            .gap(px(6.))
            .child(div().flex_none().w(px(24.)).flex().justify_center().child(Icon::Eraser.render(px(12.), hsla(p.info))))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Your next message starts a fresh session"))
                    .child(div().min_w_0().truncate().text_color(hsla(p.muted_foreground)).child("The agent won't see earlier messages in this thread.")),
            )
            .child(div().flex_none().ml_auto().child(dismiss));
        Some(banner_surface(false, content, cx).id("fresh-session-banner").debug_selector(|| "fresh-session-banner".to_owned()).into_any_element())
    }

    fn sync_prompt_suggestion(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let enabled = state
            .server_config
            .config
            .as_ref()
            .is_some_and(|c| c.settings.prompt_suggestions_enabled);
        let Some(Target::Thread(thread_id)) = self.snapshot.target.as_ref().filter(|_| enabled && self.fixed.is_none()) else {
            self.prompt_suggestion = None;
            return;
        };
        let Some(client) = state.client_handle() else {
            self.prompt_suggestion = None;
            return;
        };
        let environment = self.environment_id(cx);
        let connection_id = state.connection_id();
        let generation = Some(*client.connection_generation().borrow());
        let same = self.prompt_suggestion.as_ref().is_some_and(|w| {
            w.thread_id == *thread_id
                && w.environment == environment
                && w.connection_id == connection_id
                && w.generation == generation
        });
        if same {
            return;
        }
        let runtime = state.runtime().clone();
        let watched_thread = thread_id.clone();
        let watched_env = environment.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<rules::PromptSuggestion>>();
        let net = runtime.spawn(async move {
            let payload = serde_json::json!({ "threadId": watched_thread });
            let mut delay = std::time::Duration::from_secs(1);
            loop {
                if let Ok(mut stream) = client.stream(rules::SUBSCRIBE_PROMPT_SUGGESTION, payload.clone()).await {
                    while let Some(item) = stream.receiver.recv().await {
                        match item {
                            Ok(value) => {
                                if let Ok(suggestion) = serde_json::from_value::<Option<rules::PromptSuggestion>>(value) {
                                    delay = std::time::Duration::from_secs(1);
                                    if tx.send(suggestion).is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(t3_client::session::RpcError::Closed) => break,
                            Err(_) => {}
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(15));
            }
        });
        let thread_key = thread_id.clone();
        let ui = cx.spawn(async move |this, cx| {
            while let Some(suggestion) = rx.recv().await {
                let keep = this.update(cx, |composer, cx| {
                    let Some(watcher) = composer.prompt_suggestion.as_mut() else { return false };
                    if watcher.thread_id != thread_key || watcher.environment != watched_env {
                        return false;
                    }
                    if watcher.suggestion != suggestion {
                        watcher.suggestion = suggestion;
                        cx.notify();
                    }
                    true
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        });
        self.prompt_suggestion = Some(PromptSuggestionWatcher {
            thread_id: thread_id.clone(),
            environment,
            connection_id,
            generation,
            suggestion: None,
            _net: net,
            _ui: ui,
        });
    }

    pub(super) fn offered_prompt_suggestion(&self, cx: &App) -> Option<&str> {
        let Target::Thread(thread_id) = self.snapshot.target.as_ref()? else { return None };
        let watcher = self.prompt_suggestion.as_ref().filter(|w| w.thread_id == *thread_id)?;
        let suggestion = watcher.suggestion.as_ref()?;
        let state = self.state.read(cx);
        let shell_latest_run_id = state.shell.thread(thread_id).and_then(|t| t.latest_run_id.as_deref());
        let resolved_run = state
            .thread_state(thread_id)
            .projection
            .as_ref()
            .and_then(|p| rules::resolve_prompt_suggestion_run(shell_latest_run_id, &p.runs));
        let dismissed = self
            .dismissed_prompt_suggestions
            .contains(&(watcher.environment.clone(), suggestion.run_id.clone()));
        let prompt_empty = !self.has_text;
        let has_non_prompt_content = !self.staged().is_empty()
            || self.has_context_records(&self.prompt(cx))
            || self.current_reply_to().is_some();
        rules::offered_prompt_suggestion(
            Some(suggestion),
            rules::PromptSuggestionOfferInput {
                latest_run_id: resolved_run.map(|(id, _)| id),
                latest_run_status: resolved_run.map(|(_, status)| status),
                prompt_empty,
                has_non_prompt_content,
                has_pending_approvals: !self.snapshot.pending.approvals.is_empty(),
                has_pending_inputs: !self.snapshot.pending.user_inputs.is_empty(),
                running: self.snapshot.running,
                sending: self.sending,
                fresh_session_armed: self.is_fresh_session_armed(cx),
                dismissed,
            },
        )
    }

    pub(super) fn accept_prompt_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.menu_open() || self.stash_state.open || self.workspace.open || self.is_rewinding(cx) {
            return false;
        }
        let Some(suggestion) = self.offered_prompt_suggestion(cx).map(str::to_owned) else {
            return false;
        };
        self.set_prompt(&suggestion, window, cx);
        let end = suggestion.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
        self.draft_changed(cx);
        self.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn dismiss_prompt_suggestion(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(Target::Thread(thread_id)) = self.snapshot.target.as_ref() else { return false };
        let Some(watcher) = self.prompt_suggestion.as_ref().filter(|w| w.thread_id == *thread_id) else {
            return false;
        };
        if self.offered_prompt_suggestion(cx).is_none() {
            return false;
        }
        let Some(suggestion) = watcher.suggestion.as_ref() else { return false };
        if self
            .dismissed_prompt_suggestions
            .insert((watcher.environment.clone(), suggestion.run_id.clone()))
        {
            cx.notify();
            return true;
        }
        false
    }

    pub(crate) fn redirect_printable_key(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.is_rewinding(cx)
            || self.picker_open
            || self.permissions_open
            || self.workspace.open
            || self.stash_state.open
            || self.answering.is_some()
            || self.snapshot.target.is_none()
            || self.input.read(cx).focus_handle(cx).is_focused(window)
        {
            return false;
        }
        self.focus(window, cx);
        self.input.update(cx, |input, cx| {
            input.replace(text.to_owned(), window, cx);
        });
        true
    }

    pub(super) fn has_non_prompt_content(&self, prompt: &str, staged: &[t3_core::composer::attachments::Attachment]) -> bool {
        !staged.is_empty() || self.has_context_records(prompt) || self.current_reply_to().is_some()
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
                            "options": [{
                                "label": "Fast",
                                "value": "fast",
                                "description": "Use fast mode",
                                "preview": "Line 1 preview\nLine 2 preview"
                            }]
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

        // Typing custom text then picking an option displaces the text and emits the info toast;
        // restoring that text as a custom answer (undo) withdraws it from the stash so it isn't double-submitted.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("my typed answer", window, cx);
                composer.on_prompt_change(cx);
                assert!(composer.pick_by_digit(1, window, cx));
                assert_eq!(composer.stash.as_deref(), Some("my typed answer"));
                composer.set_prompt("my typed answer", window, cx);
                composer.on_prompt_change(cx);
                assert_eq!(composer.stash.as_deref(), Some(""));
            });
        });
        let toast_title = state.read_with(vcx, |state, _| state.toasts.latest().map(|t| t.title.to_string()));
        assert_eq!(toast_title.as_deref(), Some("Your typed text was set aside"));

        // Cancelling the pending request restores unsent custom answer text + thread chip to the thread draft.
        let thread_ctx = t3_core::composer::menu::ThreadContext::new("env-1", "t-ref", "RefThread");
        let answer_with_chip = format!("unsent answer {}", thread_ctx.reference());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt(&answer_with_chip, window, cx);
                composer.seed_mentions(&Target::Thread("t-q".to_owned()), vec![thread_ctx.clone()]);
                composer.on_prompt_change(cx);
            });
        });
        state.update(vcx, |state, cx| {
            let cancelled: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 2,
                "hasMoreHistory": false,
                "projection": {
                    "thread": {
                        "id": "t-q", "projectId": "checkout-a", "title": "Question Thread",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:06.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t-q"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "cancelled",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": "2026-04-01T00:00:01.000Z",
                        "completedAt": "2026-04-01T00:00:06.000Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [],
                    "runtimeRequests": [{
                        "id": "q-req-1",
                        "kind": "user_input",
                        "status": "cancelled",
                        "responseCapability": { "type": "live" },
                        "createdAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "messages": [], "plans": [], "turnItems": [],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [],
                    "updatedAt": "2026-04-01T00:00:06.000Z"
                }
            }"#).unwrap();
            state.thread.apply(cancelled);
            cx.emit(AppEvent::Thread);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        composer.read_with(vcx, |composer, cx| {
            assert_eq!(composer.prompt(cx), answer_with_chip);
            let mentions = composer.mentions(&Target::Thread("t-q".to_owned()));
            assert!(mentions.iter().any(|m| m.thread_id == "t-ref"));
        });
    }

    #[gpui::test]
    fn prompt_length_validation_blocks_oversized_send_and_clears_on_edit(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
            "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
            "projection": {
                "thread": {
                    "id": "t-len", "projectId": "checkout-a", "title": "Length",
                    "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                    "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                    "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                    "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t-len"}
                },
                "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [], "updatedAt": "2026-04-01T00:00:05.000Z"
            }
        }"#).unwrap();
        state.update(cx, |state, cx| {
            state.select_thread("t-len", cx);
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
        });
        let (composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));
        vcx.update(|window, cx| {
            let over = "x".repeat(rules::MAX_PROMPT_LENGTH + 3);
            composer.update(cx, |composer, cx| {
                composer.set_prompt(&over, window, cx);
                composer.submit(rules::SubmitIntent::Foreground, window, cx);
                assert!(!composer.sending);
                assert_eq!(
                    composer.prompt_length_error.as_deref(),
                    Some("Prompt is 3 characters over the 120,000-character limit. Shorten or split it before sending.")
                );
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("prompt-length-validation").is_some());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("short now", window, cx);
                composer.on_prompt_change(cx);
                assert_eq!(composer.prompt_length_error, None);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("prompt-length-validation").is_none());
    }

    #[gpui::test]
    fn picker_renders_ultracode_and_pi_tiers_and_toggles_multi_model_draft(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            let config = state.server_config.config.as_mut().unwrap();
            config.environment.capabilities.required_worktree_bootstrap = Some(true);
            config.settings.provider_instances = serde_json::from_value(serde_json::json!({
                "pi": { "driver": "pi", "enabled": true }
            }))
            .unwrap();
            config.providers = serde_json::from_value(serde_json::json!([
                {
                    "instanceId": "claudeAgent",
                    "driver": "claudeAgent",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated" },
                    "models": [{
                        "slug": "claude-sonnet-4-6",
                        "name": "Claude Sonnet 4.6",
                        "shortName": "Sonnet 4.6",
                        "isDefault": true,
                        "capabilities": {
                            "optionDescriptors": [
                                { "id": "effort", "label": "Effort", "type": "select", "options": [
                                    { "id": "high", "label": "High", "isDefault": true },
                                    { "id": "xhigh", "label": "XHigh" }
                                ]},
                                { "id": "ultracode", "label": "Ultracode", "type": "boolean", "currentValue": false }
                            ]
                        }
                    }]
                },
                {
                    "instanceId": "pi",
                    "driver": "pi",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated" },
                    "models": [{
                        "slug": "pi-fast",
                        "name": "Pi Fast",
                        "capabilities": {
                            "optionDescriptors": [
                                { "id": "serviceTier", "label": "Speed", "type": "select", "currentValue": "flex", "options": [
                                    { "id": "default", "label": "Standard" },
                                    { "id": "fast", "label": "Fast" },
                                    { "id": "flex", "label": "Ultrafast" }
                                ]}
                            ]
                        }
                    }]
                }
            ]))
            .unwrap();
            state.start_draft("checkout-a", cx);
            cx.emit(AppEvent::ServerConfig);
        });
        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let picker = self.0.read(cx).picker.clone();
                let footer = picker.update(cx, |picker, cx| picker.render_footer(cx).into_any_element());
                let trigger = self.0.update(cx, |composer, cx| {
                    let resolved = composer.resolve(cx);
                    composer.render_model_trigger(&resolved, cx)
                });
                gpui_component::v_flex().w(gpui::px(720.)).child(trigger).child(footer)
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                let input = composer.picker_input(cx);
                composer.picker.update(cx, |picker, cx| picker.update_input(input, false, cx));
                composer.on_picker(&PickerEvent::Options(vec![
                    traits::OptionSelection { id: "effort".into(), value: traits::OptionValue::Str("high".into()) },
                    traits::OptionSelection { id: "ultracode".into(), value: traits::OptionValue::Bool(true) },
                ]), window, cx);
                let input = composer.picker_input(cx);
                composer.picker.update(cx, |picker, cx| picker.update_input(input, false, cx));
                let resolved = composer.resolve(cx);
                let suffix = traits::effort_suffix(&resolved.driver, &resolved.descriptors);
                assert!(suffix.ultracode);
                assert_eq!(suffix.word.as_deref(), Some("High"));
                // Re-picking the active model preserves its options (`useComposerSelection.ts`).
                composer.on_picker(
                    &PickerEvent::Model { instance_id: "claudeAgent".into(), model: "claude-sonnet-4-6".into() },
                    window,
                    cx,
                );
                let repicked = composer.resolve(cx);
                assert!(repicked.model.as_ref().and_then(|m| m.options.as_ref()).is_some());
                assert!(traits::effort_suffix(&repicked.driver, &repicked.descriptors).ultracode);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("ultracode-toggle").is_some());

        // Switch to Pi model -> footer exposes Pi's `serviceTier` speed track and suffix reflects Ultrafast.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.on_picker(&PickerEvent::Model { instance_id: "pi".into(), model: "pi-fast".into() }, window, cx);
                let input = composer.picker_input(cx);
                composer.picker.update(cx, |picker, cx| picker.update_input(input, false, cx));
                let resolved = composer.resolve(cx);
                let suffix = traits::effort_suffix(&resolved.driver, &resolved.descriptors);
                assert!(suffix.ultrafast);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("speed-track").is_some());

        // Shift-toggle adds a second model into multi-model mode and keeps both selected until single-picked.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.on_picker(
                    &PickerEvent::ToggleModel { instance_id: "claudeAgent".into(), model: "claude-sonnet-4-6".into() },
                    window,
                    cx,
                );
                let resolved = composer.resolve(cx);
                assert_eq!(resolved.multi_models.len(), 2);
                assert_eq!(resolved.multi_model_label.as_deref(), Some("Pi Fast, Sonnet 4.6"));
            });
            window.draw(cx).clear();
        });
    }

    #[gpui::test]
    fn shift_enter_indents_code_blocks_without_continuing_lists_and_stash_highlights_first(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, _| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.environment.capabilities.fresh_context = Some(true);
            }
        });
        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                gpui::div()
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                // Shift+Enter inside a fenced code block preserves leading indentation (`CO-RICH`).
                composer.set_prompt("```rust\n  let x = 1;\n```", window, cx);
                let caret = "```rust\n  let x = 1;".len();
                composer.input.update(cx, |input, cx| input.set_selected_range(caret..caret, cx));
                composer.on_enter(false, true, window, cx);
                assert_eq!(composer.prompt(cx), "```rust\n  let x = 1;\n  \n```");

                // Shift+Enter on a list item inserts a plain newline instead of continuing `- ` (`#36285`).
                composer.set_prompt("- first", window, cx);
                let end = "- first".len();
                composer.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
                composer.on_enter(false, true, window, cx);
                assert_eq!(composer.prompt(cx), "- first\n");

                // Stash menu highlights its first entry when toggled open (`CO-STASH`).
                composer.stash_state.store.push(t3_core::composer::stash::StashEntry {
                    id: "s-first".into(),
                    created_at: 100,
                    prompt: "stashed prompt".into(),
                    attachments: Vec::new(),
                    threads: Vec::new(),
                    terminals: Vec::new(),
                    review_comments: Vec::new(),
                    context_records: Vec::new(),
                    reply_to: None,
                });
                composer.toggle_stash_menu(cx);
                assert!(composer.stash_state.open);
                assert_eq!(composer.stash_state.highlighted.as_deref(), Some("s-first"));

                // #291 (`/clear`) remains false on Draft (requires a thread with existing history).
                assert!(!composer.fresh_session_available(cx));
            });
        });
        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn paste_context_imports_local_thread_urls_and_image_preparation_downscales(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
                gpui::div()
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());

        // Plain hosted thread link paste without clipboard metadata imports matching env thread and leaves foreign links intact.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                let local_env = composer.environment_id(cx);
                let raw = format!(
                    "See https://app.t3.codes/threads/{local_env}/menu-thread and https://app.t3.codes/threads/foreign-env/other"
                );
                assert!(composer.paste_context(&raw, "", window, cx));
                let prompt = composer.prompt(cx);
                assert!(prompt.contains("t3-context://v1/thread/thread_menu-thread"), "local thread rewritten: {prompt}");
                assert!(prompt.contains("https://app.t3.codes/threads/foreign-env/other"), "foreign link kept: {prompt}");

                // Capability loss releases unpersisted uploads/images while preserving ready persisted files.
                composer.seed_attachments(
                    &Target::Thread("menu-thread".into()),
                    vec![
                        t3_core::composer::attachments::Attachment {
                            id: "file-ready".into(),
                            kind: t3_core::composer::attachments::AttachmentKind::File,
                            name: "spec.md".into(),
                            mime_type: "text/markdown".into(),
                            size_bytes: 64,
                            pasted_text: false,
                            upload: t3_core::composer::attachments::Upload::Ready { attachment_id: "srv-file".into() },
                        },
                        t3_core::composer::attachments::Attachment {
                            id: "img-ready".into(),
                            kind: t3_core::composer::attachments::AttachmentKind::Image,
                            name: "shot.png".into(),
                            mime_type: "image/png".into(),
                            size_bytes: 64,
                            pasted_text: false,
                            upload: t3_core::composer::attachments::Upload::Ready { attachment_id: "srv-img".into() },
                        },
                    ],
                    false,
                    cx,
                );
                assert_eq!(composer.staged_for(&Target::Thread("menu-thread".into())).len(), 2);
            });
        });

        vcx.update(|window, cx| {
            state.update(cx, |state, _| {
                if let Some(config) = state.server_config.config.as_mut() {
                    config.environment.capabilities.file_attachments = None;
                }
            });
            composer.update(cx, |composer, cx| {
                composer.reconcile_upload_capabilities(cx);
                let remaining = composer.staged_for(&Target::Thread("menu-thread".into()));
                assert_eq!(remaining.len(), 1);
                assert_eq!(remaining[0].attachment.id, "file-ready");
                let _ = window;
            });
        });

        // Oversized > 2000px image compresses cleanly within IMAGE_ATTACHMENT_MAX_EDGE.
        let large_img = ::image::DynamicImage::new_rgb8(2400, 1200);
        let mut png_bytes = Vec::new();
        large_img.write_to(&mut std::io::Cursor::new(&mut png_bytes), ::image::ImageFormat::Png).unwrap();
        let prepared = attachments::prepare_image_bytes("big.png".into(), "image/png".into(), png_bytes, false).unwrap();
        let dims = t3_core::composer::attachments::image::read_image_dimensions(&prepared.bytes).unwrap();
        assert_eq!(dims, (2000, 1000));

        // 1. Offline-staged attachment fails with "Not connected", then await_target_uploads (retry_on_send)
        //    enqueues it when client is available, and sync_upload_states_for reconciles settled queue state immediately.
        let target = Target::Thread("menu-thread".into());
        vcx.update(|_, cx| {
            composer.update(cx, |composer, cx| {
                // Re-enable file attachments and simulate offline disconnect during staging.
                composer.state.update(cx, |state, _| {
                    if let Some(config) = state.server_config.config.as_mut() {
                        config.environment.capabilities.file_attachments = Some(
                            t3_core::server_config::FileAttachmentsCapability { max_upload_bytes: 10_000_000 },
                        );
                    }
                });
                let saved_config = composer.state.read(cx).server_config.clone();
                let offline_client = composer.state.read(cx).client_handle().unwrap();
                composer.state.update(cx, |state, cx| {
                    state.clear_connection_state(cx);
                    state.server_config = saved_config.clone();
                });
                composer.stage_picked_for_test(&target, "offline.txt".into(), "text/plain".into(), b"offline bytes".to_vec(), cx);
                let offline_id = composer
                    .staged_for(&target)
                    .iter()
                    .find(|s| s.attachment.name == "offline.txt")
                    .map(|s| s.attachment.id.clone())
                    .unwrap();
                assert!(matches!(
                    composer.staged_for(&target).iter().find(|s| s.attachment.id == offline_id).unwrap().attachment.upload,
                    t3_core::composer::attachments::Upload::Failed { ref reason } if reason == "Not connected"
                ));

                // Restore client and mark another queue job Ready directly to verify sync_upload_states_for + retire_sent.
                composer.state.update(cx, |state, cx| {
                    state.install_active_environment(
                        offline_client.clone(),
                        t3_core::shell::ShellState::default(),
                        saved_config,
                        t3_client::supervisor::ConnectionStatus::Connected,
                        cx,
                    );
                });
                let wait_opt = composer.await_target_uploads(&target, std::slice::from_ref(&offline_id), cx);
                assert!(wait_opt.is_some(), "offline-failed attachment must enqueue and yield a wait task on retry_on_send");
                drop(wait_opt);
                let queue_key = format!("{}::{offline_id}", drafts::key(&target));
                assert!(composer.state.read(cx).uploads.state(&queue_key).is_some());

                // Also verify pre-ready queue state syncs synchronously via sync_upload_states_for & retire_sent preserves siblings.
                let sent_key = format!("{}::file-ready", drafts::key(&target));
                composer.seed_queue_ready_for_test(&target, "file-ready", "srv-file", cx);
                assert!(composer.state.read(cx).uploads.state(&sent_key).is_some());
                let taken = composer.take_attachments(&target);
                let (sent_only, siblings): (Vec<_>, Vec<_>) = taken.into_iter().partition(|s| s.attachment.id == "file-ready");
                composer.restore_attachments(&target, siblings);
                composer.retire_sent_attachments(&target, &sent_only, cx);
                assert!(composer.state.read(cx).uploads.state(&sent_key).is_none());
                assert!(composer.state.read(cx).uploads.state(&queue_key).is_some());

                // Verify sync_upload_states_for reconciles Ready queue state onto staged synchronously without waiting for upload_pump.
                composer.find_staged_upload_ready_sync_test(&target, "sync-test-id", "srv-synced", cx);
                assert!(matches!(
                    composer.staged_for(&target).iter().find(|s| s.attachment.id == "sync-test-id").unwrap().attachment.upload,
                    t3_core::composer::attachments::Upload::Ready { ref attachment_id } if attachment_id == "srv-synced"
                ));
            });
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn c4_fresh_session_compact_toggle_stop_phase_queue_and_launching_work_end_to_end(cx: &mut gpui::TestAppContext) {
        use serde_json::json;
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let stale_at = "2020-01-01T00:00:00.000Z";
        state.update(cx, |state, cx| {
            if let Some(config) = state.server_config.config.as_mut() {
                config.environment.capabilities.fresh_context = Some(true);
                config.providers = serde_json::from_value(json!([
                    {
                        "instanceId": "claudeAgent",
                        "driver": "claudeAgent",
                        "enabled": true,
                        "installed": true,
                        "status": "ready",
                        "auth": {"status": "authenticated"},
                        "models": [{"slug": "sonnet", "name": "Sonnet", "isCustom": false}],
                        "slashCommands": [{"name": "compact", "description": "Compact"}]
                    }
                ]))
                .unwrap();
            }
            // #33783: pre-launch mark puts thread in Working immediately and clears on failure.
            state.mark_launching_thread("t-launching-c4", cx);
            assert!(state.sidebar.history.launching_threads().contains("t-launching-c4"));
            state.clear_launching_thread("t-launching-c4", cx);
            assert!(!state.sidebar.history.launching_threads().contains("t-launching-c4"));

            let mut shell = state.shell.clone();
            if let Some(snapshot) = shell.snapshot.as_mut() {
                snapshot.threads.push(serde_json::from_value(json!({
                    "id": "t-c4", "projectId": "checkout-a", "title": "C4 Thread",
                    "createdAt": stale_at, "updatedAt": stale_at, "archivedAt": null,
                    "branch": null, "worktreePath": null, "lastVisitedAt": null,
                    "providerInstanceId": "claudeAgent", "modelSelection": {"instanceId": "claudeAgent", "model": "sonnet"},
                    "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId": "t-c4"},
                    "latestRunId": "r1", "activeRunId": null, "status": "idle", "hasActionableProposedPlan": false,
                    "itemCount": 1, "visibleItemCount": 1
                })).unwrap());
            }
            state.shell = shell;
            state.select_thread("t-c4", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": false,
                "projection": {
                    "thread": {
                        "id": "t-c4", "projectId": "checkout-a", "title": "C4 Thread",
                        "createdAt": "2020-01-01T00:00:00.000Z", "updatedAt": "2020-01-01T00:00:00.000Z", "archivedAt": null,
                        "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "claudeAgent", "modelSelection": {"instanceId": "claudeAgent", "model": "sonnet"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId": "t-c4"},
                        "activeProviderThreadId": "pt1"
                    },
                    "runs": [
                        {
                            "id": "r1", "ordinal": 1, "providerInstanceId": "claudeAgent",
                            "modelSelection": {"instanceId": "claudeAgent", "model": "sonnet"},
                            "providerThreadId": "pt1", "userMessageId": "m1", "rootNodeId": null,
                            "status": "interrupted", "requestedAt": "2020-01-01T00:00:00.000Z", "startedAt": "2020-01-01T00:00:00.000Z", "completedAt": "2020-01-01T00:00:00.000Z"
                        },
                        {
                            "id": "r2", "ordinal": 2, "providerInstanceId": "claudeAgent",
                            "modelSelection": {"instanceId": "claudeAgent", "model": "sonnet"},
                            "providerThreadId": "pt1", "userMessageId": "m2", "rootNodeId": null,
                            "status": "queued", "requestedAt": "2020-01-01T00:00:00.000Z", "startedAt": null, "completedAt": null,
                            "queuePosition": 1, "freshContext": true
                        }
                    ],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [{"id": "pt1", "appThreadId": "t-c4", "updatedAt": "2020-01-01T00:00:00.000Z"}],
                    "providerTurns": [{"id": "turn1", "providerThreadId": "pt1", "tokenUsage": {"usedTokens": 250000, "maxTokens": 1000000, "updatedAt": "2020-01-01T00:00:00.000Z"}}],
                    "runtimeRequests": [],
                    "messages": [
                        {"id": "m1", "text": "prior prompt", "attachments": []},
                        {"id": "m2", "text": "queued fresh", "attachments": [], "createdBy": "agent", "creationSource": "cli", "linkedSender": {"environmentId": "env-missing", "threadId": "t-rem", "title": "Remote Agent"}}
                    ],
                    "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
                    "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [{
                        "position": 0, "visibility": "local", "sourceThreadId": "t-c4", "sourceItemId": "i1",
                        "item": {
                            "id": "i1", "threadId": "t-c4", "runId": "r1", "nodeId": "n1", "parentItemId": null,
                            "ordinal": 1, "status": "completed", "title": null, "startedAt": "2020-01-01T00:00:00.000Z",
                            "completedAt": "2020-01-01T00:00:00.000Z", "updatedAt": "2020-01-01T00:00:00.000Z",
                            "type": "user_message", "messageId": "m1", "text": "prior prompt", "inputIntent": "turn", "attachments": []
                        }
                    }],
                    "updatedAt": "2020-01-01T00:00:00.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
            cx.emit(AppEvent::Selection);
        });

        let (composer, vcx) = cx.add_window_view(|window, cx| Composer::new(state.clone(), window, cx));
        vcx.update(|window, cx| window.draw(cx).clear());
        // Queued linked-sender card renders static folded row & unknown remote env omits Open thread.
        assert!(vcx.debug_bounds("queued-fold-0").is_some());
        assert!(vcx.debug_bounds("queued-open-thread-0").is_none());

        // #291: /clear command arms fresh session for existing thread with capability & matches seq on consumption.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                assert!(composer.fresh_session_available(cx));
                assert!(composer.fresh_session_command_available(cx));
                assert!(composer.snapshot.can_resume);
                assert_eq!(composer.snapshot.resumable_run_id.as_deref(), Some("r1"));
                composer.set_prompt("/clear", window, cx);
                composer.submit(rules::SubmitIntent::Foreground, window, cx);
                assert!(composer.is_fresh_session_armed(cx));
                assert!(composer.prompt(cx).is_empty());
                let first_arm = composer.fresh_session_arm(cx).expect("armed");
                composer.arm_fresh_session(cx);
                let second_arm = composer.fresh_session_arm(cx).expect("re-armed");
                assert_ne!(first_arm.2, second_arm.2);
                composer.consume_fresh_session_arm(first_arm, cx);
                assert!(composer.is_fresh_session_armed(cx), "stale arm must not consume newer arm");
                composer.consume_fresh_session_arm(second_arm, cx);
                assert!(!composer.is_fresh_session_armed(cx));

                // #280: idle stale Claude thread offers Compact toggle when typing prompt.
                composer.set_prompt("continue debugging", window, cx);
                let resolved = composer.resolve(cx);
                assert_eq!(composer.compact_before_send_tokens(&resolved, cx), Some(250_000.0));
                assert!(!composer.is_keep_full_history(cx));
                composer.toggle_keep_full_history(cx);
                assert!(composer.is_keep_full_history(cx));
                composer.toggle_keep_full_history(cx);
                assert!(!composer.is_keep_full_history(cx));
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("compact-before-send").is_some());

        // #20809: Stop transition sets Pending and retains Stop button beside Send while typing, then lands on stop completion.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.snapshot.running = true;
                composer.snapshot.can_interrupt = true;
                composer.snapshot.active_run_id = Some("r-live".into());
                composer.request_stop(cx);
                assert_eq!(composer.stop_phase, StopPhase::Pending);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("stop").is_some());
        assert!(vcx.debug_bounds("send").is_some());
        vcx.update(|_, cx| {
            composer.update(cx, |composer, cx| {
                composer.snapshot.running = false;
                composer.snapshot.can_interrupt = false;
                composer.sync_stop_phase(cx);
                assert_eq!(composer.stop_phase, StopPhase::Landed);
            });
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn c4_worktree_base_branch_picker_search_pagination_and_host_visibility_parity(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            state.environments.environments = vec![
                t3_client::store::SavedEnvironment {
                    id: "local".into(),
                    environment_id: Some("env-local".into()),
                    label: "MacBook Pro".into(),
                    disabled: false,
                    http_base: "http://127.0.0.1:1".into(),
                    access_token: None,
                    server_version: None,
                    local: true,
                    ssh: None,
                },
                t3_client::store::SavedEnvironment {
                    id: "remote-1".into(),
                    environment_id: Some("env-remote".into()),
                    label: "Build Box".into(),
                    disabled: false,
                    http_base: "http://127.0.0.1:2".into(),
                    access_token: None,
                    server_version: None,
                    local: false,
                    ssh: None,
                },
            ];
            state.environments.active = Some("local".into());
            let mut shell = state.shell.snapshot.clone().unwrap_or_default();
            if let Some(proj) = shell.projects.iter_mut().find(|p| p.id == "checkout-a") {
                proj.repository_identity = Some(serde_json::json!({
                    "canonicalKey": "github.com/acme/repo",
                    "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": "https://github.com/acme/repo.git" }
                }));
            }
            state.shell.apply(t3_core::shell::ShellStreamItem::Snapshot {
                snapshot: Box::new(shell),
                resolved_repository_identity_roots: None,
            });
            state.start_draft("checkout-a", cx);
        });

        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let workspace = self.0.update(cx, |composer, cx| composer.render_workspace(cx));
                gpui::div().w(gpui::px(720.)).children(workspace)
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.sync_workspace(false, cx);
                composer.workspace.is_repo = true;
                let req = composer.workspace.refs.begin("", false).unwrap();
                composer.workspace.refs.finish(
                    &req,
                    Ok(vec![t3_core::vcs::ListRefsResult {
                        is_repo: true,
                        has_primary_remote: true,
                        refs: vec![t3_core::vcs::VcsRef {
                            name: "main".into(),
                            current: true,
                            is_default: true,
                            is_remote: Some(false),
                            remote_name: None,
                            worktree_path: None,
                        }],
                        next_cursor: Some(100),
                        total_count: 150,
                    }]),
                );
            });
            window.draw(cx).clear();
        });

        // Finding 3: single/non-fleet primary host must stay hidden even when >1 saved envs exist.
        vcx.update(|_, cx| {
            let c = composer.read(cx);
            assert!(c.host_options(cx).active.is_none());
        });
        assert!(vcx.debug_bounds("composer-host").is_none());
        assert!(vcx.debug_bounds("composer-host-indicator").is_none());

        // Finding 3: remote non-fleet host renders muted non-interactive indicator (`composer-host-indicator`).
        vcx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.environments.environments[0].local = false;
                state.environments.environments[0].label = "Remote Box".into();
                cx.emit(AppEvent::Shell);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("composer-host-indicator").is_some());
        assert!(vcx.debug_bounds("composer-host").is_none());

        // Restore primary and test fleet mode with `add_targets` when another connected machine lacks the project.
        vcx.update(|window, cx| {
            state.update(cx, |state, cx| {
                state.environments.environments[0].local = true;
                state.environments.environments[0].label = "MacBook Pro".into();
                let owner_local = t3_core::sidebar::grouping::EnvironmentOwner {
                    saved_id: "local".into(),
                    canonical_id: Some("env-local".into()),
                    generation: 1,
                    connection_generation: 1,
                    shell_generation: 1,
                };
                let owner_remote = t3_core::sidebar::grouping::EnvironmentOwner {
                    saved_id: "remote-1".into(),
                    canonical_id: Some("env-remote".into()),
                    generation: 1,
                    connection_generation: 1,
                    shell_generation: 1,
                };
                let mut config_remote = t3_core::server_config::ServerConfigState::default();
                let cfg: t3_core::server_config::ServerConfig = serde_json::from_value(serde_json::json!({
                    "environment": { "environmentId": "env-remote", "label": "Build Box", "platform": { "os": "linux", "arch": "x64" } },
                    "auth": { "policy": "loopback-auto" }
                })).unwrap();
                config_remote.config = Some(cfg);
                let remote_grants: t3_core::auth::AuthSessionState = serde_json::from_value(serde_json::json!({
                    "authenticated": true,
                    "scopes": ["source-control:write", "orchestration:read"]
                })).unwrap();
                let mut remote_shell = t3_core::shell::ShellState::default();
                remote_shell.synchronized = true;
                state.fleet = std::sync::Arc::new(t3_client::environments::FleetView {
                    revision: 1,
                    environments: vec![
                        std::sync::Arc::new(t3_client::environments::EnvironmentView {
                            summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                                owner_local,
                                &state.shell,
                                &state.server_config,
                            )),
                            status: t3_client::supervisor::ConnectionStatus::Connected,
                            config: state.server_config.clone(),
                            grants: Some(remote_grants.clone()),
                            settings_revision: 0,
                            keybindings_revision: 0,
                        }),
                        std::sync::Arc::new(t3_client::environments::EnvironmentView {
                            summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                                owner_remote,
                                &remote_shell,
                                &config_remote,
                            )),
                            status: t3_client::supervisor::ConnectionStatus::Connected,
                            config: config_remote,
                            grants: Some(remote_grants),
                            settings_revision: 0,
                            keybindings_revision: 0,
                        }),
                    ],
                });
                state.sidebar.grouping_projects = state.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect();
                state.sidebar.grouping_threads = state.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter().cloned()).collect();
                state.rebuild_sidebar(cx);
                cx.emit(AppEvent::Shell);
            });
            window.draw(cx).clear();
            let host = composer.read(cx).host_options(cx);
            assert!(host.can_pick);
            assert_eq!(host.options.len(), 1);
            assert_eq!(host.add_targets.len(), 1);
            assert_eq!(host.add_targets[0].saved_id, "remote-1");
        });
        assert!(vcx.debug_bounds("composer-host").is_some());

        // Finding 1: WorktreeBaseBranchPicker pagination status, empty state, and search reset on close.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.mutate_draft_workspace(cx, |ws| ws.mode = t3_core::server_config::ThreadEnvMode::Worktree);
            });
            window.draw(cx).clear();
        });
        assert!(vcx.debug_bounds("composer-base-branch").is_some());
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.open_base_branch_picker(window, cx);
                assert!(composer.workspace.branch_open);
                assert!(composer.workspace.base_search.is_some());
                assert_eq!(composer.base_branch_status_text().as_deref(), Some("Showing 1 of 150 refs"));

                let next = composer.workspace.refs.begin("", true).unwrap();
                composer.workspace.refs.finish(
                    &next,
                    Ok(vec![t3_core::vcs::ListRefsResult {
                        is_repo: true,
                        has_primary_remote: true,
                        refs: vec![t3_core::vcs::VcsRef {
                            name: "origin/release".into(),
                            current: false,
                            is_default: false,
                            is_remote: Some(true),
                            remote_name: Some("origin".into()),
                            worktree_path: None,
                        }],
                        next_cursor: None,
                        total_count: 2,
                    }]),
                );
                assert_eq!(composer.workspace.refs.refs.len(), 2);
                assert_eq!(composer.base_branch_status_text(), None);

                let empty_req = composer.workspace.refs.begin("missing-ref", false).unwrap();
                composer.workspace.refs.finish(
                    &empty_req,
                    Ok(vec![t3_core::vcs::ListRefsResult {
                        is_repo: true,
                        has_primary_remote: true,
                        refs: Vec::new(),
                        next_cursor: None,
                        total_count: 0,
                    }]),
                );
                assert!(composer.workspace.refs.refs.is_empty());
                assert_eq!(composer.workspace.refs.error, None);

                composer.workspace.base_search.as_ref().unwrap().update(cx, |input, cx| input.set_value("missing-ref", window, cx));
                composer.close_base_branch_picker(false, window, cx);
                assert!(!composer.workspace.branch_open);
                assert_eq!(composer.workspace.base_search.as_ref().unwrap().read(cx).value(), "");
            });
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn c4_composer_queue_and_host_keybindings_dispatch_and_tooltips(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));

        cx.update(|cx| {
            crate::keymap::init(cx);
            assert_eq!(crate::keymap::action_for("composer.cycleHost").as_any().type_id(), std::any::TypeId::of::<CycleHost>());
            assert_eq!(crate::keymap::action_for("composer.sendAlternate").as_any().type_id(), std::any::TypeId::of::<SendAlternate>());
            assert_eq!(crate::keymap::action_for("thread.steerQueuedMessage").as_any().type_id(), std::any::TypeId::of::<SteerQueuedMessage>());
            assert_eq!(crate::keymap::action_for("thread.editQueuedMessage").as_any().type_id(), std::any::TypeId::of::<EditQueuedMessage>());
            assert!(crate::keymap::label("thread.steerQueuedMessage", cx).is_some());
            assert!(
                crate::keymap::label_in(
                    "thread.editQueuedMessage",
                    &t3_core::keybindings::ShortcutMatchContext::default().with("composerFocus", true),
                    cx,
                )
                .is_some()
            );
            assert!(
                crate::keymap::label_in(
                    "composer.sendAlternate",
                    &t3_core::keybindings::ShortcutMatchContext::default().with("composerFocus", true).with("turnRunning", true),
                    cx,
                )
                .is_some()
            );
        });

        let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
            "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
            "projection": {
                "thread": {
                    "id": "t1", "projectId": "checkout-a", "title": "T1",
                    "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                    "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                    "providerInstanceId": "codex", "modelSelection": {"instanceId":"codex","model":"m"},
                    "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                    "activeRunId": "run-active"
                },
                "runs": [
                    { "id": "run-active", "threadId": "t1", "ordinal": 1, "providerInstanceId": "codex", "modelSelection": {"instanceId":"codex","model":"m"}, "status": "running", "userMessageId": "m0", "activeAttemptId": "att-0", "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": "2026-04-01T00:00:01.000Z", "completedAt": null },
                    { "id": "run-q1", "threadId": "t1", "ordinal": 2, "queuePosition": 1, "providerInstanceId": "codex", "modelSelection": {"instanceId":"codex","model":"m"}, "status": "queued", "userMessageId": "m1", "requestedAt": "2026-04-01T00:00:02.000Z", "startedAt": null, "completedAt": null },
                    { "id": "run-q2", "threadId": "t1", "ordinal": 3, "queuePosition": 2, "providerInstanceId": "codex", "modelSelection": {"instanceId":"codex","model":"m"}, "status": "queued", "userMessageId": "m2", "requestedAt": "2026-04-01T00:00:03.000Z", "startedAt": null, "completedAt": null }
                ],
                "attempts": [], "nodes": [], "subagents": [],
                "providerSessions": [{ "id": "ps1", "threadId": "t1", "providerInstanceId": "codex", "status": "running", "capabilities": { "turns": { "supportsActiveSteering": true } } }],
                "providerThreads": [],
                "providerTurns": [{ "id": "pt1", "threadId": "t1", "runAttemptId": "att-0", "status": "running" }],
                "runtimeRequests": [],
                "messages": [
                    { "id": "m0", "role": "user", "text": "active" },
                    { "id": "m1", "role": "user", "text": "First queued" },
                    { "id": "m2", "role": "user", "text": "Latest queued" }
                ],
                "plans": [], "turnItems": [], "checkpointScopes": [],
                "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [], "updatedAt": "2026-04-01T00:00:05.000Z"
            }
        }"#).unwrap();
        state.update(cx, |state, cx| {
            state.client_settings.follow_up_behavior = t3_core::composer::FollowUpBehavior::Steer;
            let config = state.server_config.config.get_or_insert_with(|| {
                serde_json::from_value(serde_json::json!({
                    "environment": { "environmentId": "env-local", "label": "MacBook Pro", "platform": { "os": "darwin", "arch": "arm64" } },
                    "auth": { "policy": "loopback-auto" }
                }))
                .unwrap()
            });
            config.providers = serde_json::from_value(serde_json::json!([
                {
                    "instanceId": "codex",
                    "driver": "codex",
                    "enabled": true,
                    "installed": true,
                    "status": "ready",
                    "auth": { "status": "authenticated" },
                    "models": [{ "slug": "m", "name": "GPT-5" }]
                }
            ]))
            .unwrap();
            let mut shell_snap = state.shell.snapshot.clone().unwrap_or_default();
            shell_snap.threads.push(serde_json::from_value(serde_json::json!({
                "id": "t1",
                "projectId": "checkout-a",
                "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z",
                "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "codex",
                "modelSelection": {"instanceId":"codex","model":"m"},
                "runtimeMode": "auto",
                "interactionMode": "default",
                "lineage": {"rootThreadId":"t1"},
                "status": "running",
                "activeRunId": "run-active",
                "hasActionableProposedPlan": false,
                "itemCount": 0,
                "visibleItemCount": 0
            })).unwrap());
            state.shell.apply(t3_core::shell::ShellStreamItem::Snapshot {
                snapshot: Box::new(shell_snap),
                resolved_repository_identity_roots: None,
            });
            state.select_thread("t1", cx);
            state.thread.apply(snapshot);
            cx.emit(AppEvent::ServerConfig);
            cx.emit(AppEvent::Shell);
            cx.emit(AppEvent::Thread);
        });
        struct Host(Entity<Composer>);
        impl gpui::Render for Host {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let queue = self.0.update(cx, |composer, cx| composer.render_queue(cx));
                gpui::div().w(gpui::px(720.)).children(queue)
            }
        }
        let (host, vcx) = cx.add_window_view(|window, cx| Host(cx.new(|cx| Composer::new(state.clone(), window, cx))));
        let composer = host.read_with(vcx, |host, _| host.0.clone());
        vcx.update(|window, cx| window.draw(cx).clear());

        // `edit_queued_message` checks caret position: rejected when caret is not at 0..0, edits latest (`run-q2`) when at 0..0.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_prompt("drafting", window, cx);
                composer.input.update(cx, |input, cx| input.set_selected_range(3..3, cx));
                assert!(!composer.edit_queued_message(window, cx));
                assert!(composer.editing_queued().is_none());

                composer.set_prompt("", window, cx);
                composer.input.update(cx, |input, cx| input.set_selected_range(0..0, cx));
                assert!(composer.edit_queued_message(window, cx));
                assert_eq!(composer.editing_queued().map(|e| e.run_id.as_str()), Some("run-q2"));
                assert_eq!(composer.prompt(cx), "Latest queued");
                composer.cancel_queued_edit(window, cx);
                assert!(composer.editing_queued().is_none());

                // `steer_queued_message` promotes the FIRST queued run (`run-q1`).
                assert!(composer.steer_queued_message(window, cx));
            });
        });
        let req_payload = loop {
            let (m, payload, tx) = fixture.next_request().await;
            if m == t3_core::commands::method::DISPATCH_COMMAND && payload["type"] != "thread.visit" {
                let _ = tx.send(Ok(serde_json::json!({"sequence": 2})));
                break payload;
            }
            let _ = tx.send(Ok(serde_json::json!({})));
        };
        assert_eq!(req_payload["type"], "queued-message.promote-to-steer");
        assert_eq!(req_payload["queuedRunId"], "run-q1");
        vcx.run_until_parked();

        // `send_alternate` dispatches while running when `composer.sendAlternate` fires.
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                assert!(composer.snapshot.running, "thread must be running for send_alternate");
                assert!(!composer.sending, "composer should not still be sending");
                assert!(!composer.snapshot.setup_blocks_send, "setup should not block send");
                assert!(composer.resolve(cx).model.is_some(), "provider/model should resolve");
                composer.set_prompt("Alternate send", window, cx);
                composer.send_alternate(window, cx);
                assert_eq!(composer.error, None, "send_alternate should not error");
                assert!(composer.sending, "send_alternate should start sending");
            });
        });
        let alt_payload = loop {
            let (m, payload, tx) = fixture.next_request().await;
            if m == t3_core::commands::method::DISPATCH_COMMAND && payload["type"] != "thread.visit" {
                let _ = tx.send(Ok(serde_json::json!({"sequence": 3})));
                break payload;
            }
            let _ = tx.send(Ok(serde_json::json!({})));
        };
        assert_eq!(alt_payload["type"], "message.dispatch");
        assert_eq!(alt_payload["dispatchMode"]["type"], "queue_after_active");
        vcx.run_until_parked();

        // `cycle_host` switches between 2 fleet project environments.
        vcx.update(|_window, cx| {
            state.update(cx, |state, cx| {
                state.environments.environments = vec![
                    t3_client::store::SavedEnvironment {
                        id: "local".into(),
                        environment_id: Some("env-local".into()),
                        label: "MacBook Pro".into(),
                        disabled: false,
                        http_base: "http://127.0.0.1:1".into(),
                        access_token: None,
                        server_version: None,
                        local: true,
                        ssh: None,
                    },
                    t3_client::store::SavedEnvironment {
                        id: "remote-1".into(),
                        environment_id: Some("env-remote".into()),
                        label: "Build Box".into(),
                        disabled: false,
                        http_base: "http://127.0.0.1:2".into(),
                        access_token: None,
                        server_version: None,
                        local: false,
                        ssh: None,
                    },
                ];
                state.environments.active = Some("local".into());
                let owner_local = t3_core::sidebar::grouping::EnvironmentOwner {
                    saved_id: "local".into(),
                    canonical_id: Some("env-local".into()),
                    generation: 1,
                    connection_generation: 1,
                    shell_generation: 1,
                };
                let owner_remote = t3_core::sidebar::grouping::EnvironmentOwner {
                    saved_id: "remote-1".into(),
                    canonical_id: Some("env-remote".into()),
                    generation: 1,
                    connection_generation: 1,
                    shell_generation: 1,
                };
                let mut shell_remote = state.shell.snapshot.clone().unwrap_or_default();
                for proj in &mut shell_remote.projects {
                    if proj.id == "checkout-a" {
                        proj.repository_identity = Some(serde_json::json!({"canonicalKey": "github.com/acme/repo"}));
                    }
                }
                state.shell.apply(t3_core::shell::ShellStreamItem::Snapshot {
                    snapshot: Box::new(shell_remote.clone()),
                    resolved_repository_identity_roots: None,
                });
                let mut remote_shell_state = t3_core::shell::ShellState::default();
                remote_shell_state.synchronized = true;
                remote_shell_state.apply(t3_core::shell::ShellStreamItem::Snapshot {
                    snapshot: Box::new(shell_remote),
                    resolved_repository_identity_roots: None,
                });
                let mut config_remote = t3_core::server_config::ServerConfigState::default();
                config_remote.config = Some(serde_json::from_value(serde_json::json!({
                    "environment": { "environmentId": "env-remote", "label": "Build Box", "platform": { "os": "linux", "arch": "x64" } },
                    "auth": { "policy": "loopback-auto" }
                })).unwrap());
                let grants: t3_core::auth::AuthSessionState = serde_json::from_value(serde_json::json!({
                    "authenticated": true,
                    "scopes": ["source-control:write", "orchestration:read"]
                })).unwrap();
                state.fleet = std::sync::Arc::new(t3_client::environments::FleetView {
                    revision: 1,
                    environments: vec![
                        std::sync::Arc::new(t3_client::environments::EnvironmentView {
                            summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                                owner_local,
                                &state.shell,
                                &state.server_config,
                            )),
                            status: t3_client::supervisor::ConnectionStatus::Connected,
                            config: state.server_config.clone(),
                            grants: Some(grants.clone()),
                            settings_revision: 0,
                            keybindings_revision: 0,
                        }),
                        std::sync::Arc::new(t3_client::environments::EnvironmentView {
                            summary: std::sync::Arc::new(t3_core::sidebar::fleet::EnvironmentSummary::from_shell(
                                owner_remote,
                                &remote_shell_state,
                                &config_remote,
                            )),
                            status: t3_client::supervisor::ConnectionStatus::Connected,
                            config: config_remote,
                            grants: Some(grants),
                            settings_revision: 0,
                            keybindings_revision: 0,
                        }),
                    ],
                });
                state.sidebar.grouping_projects = state.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect();
                state.sidebar.grouping_threads = state.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter().cloned()).collect();
                state.rebuild_sidebar(cx);
                state.start_draft("checkout-a", cx);
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            composer.update(cx, |composer, cx| {
                assert_eq!(composer.host_options(cx).options.len(), 2);
                composer.cycle_host(window, cx);
            });
            assert_eq!(state.read(cx).environments.active.as_deref(), Some("remote-1"));
        });

        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
