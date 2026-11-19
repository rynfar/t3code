//! The pull request detail panel (`PullRequestDetailPanel.tsx`): its reads, the header with the
//! primary control and the actions menu (`PullRequestDetailActions.tsx`,
//! `PullRequestActionsMenu.tsx`) with the web's two-step confirmations
//! (`useInlineActionConfirm.ts`), the stack merge where the host runs stack actions
//! (`PullRequestStackMenu.tsx`), and the Summary, Timeline and Code tabs. It paints at once
//! from what was last read for the pull request (`usePullRequestDetail`'s held answers), applies
//! an action's reply without a re-read, writes the description's task checkboxes back, resolves
//! conversation threads, edits labels, and goes where a link's fragment points.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FontWeight, Global, IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::commands::method::DISPATCH_COMMAND;
use t3_core::pull_requests::detail::{MergeGate, PrimaryControl, StateKind, change_request_term, host_label, merge_gate, merge_method_label, primary_control, resolve_merge_method};
use t3_core::pull_requests::anchor::{Anchor, LinkedTab, anchor_matches_url, link_commit, parse_anchor, resolve_review_anchor};
use t3_core::pull_requests::chat_context::{ChatSubject, build_comment_context, build_pull_request_context};
use t3_core::pull_requests::detail::EventKind;
use t3_core::pull_requests::link::{
    LINKED_THREADS, LINKED_THREADS_POLL_SECONDS, LinkableThread, LinkedThread, LinkedThreads, SUBSCRIBE_LINKED_THREAD_REFRESHES, is_linked_to_url, link_command, linked_threads_input,
    linked_threads_label, parse_change_request_url, unlink_command, visible_linked_threads,
};
use t3_core::pull_requests::editing::{
    self, DescriptionDraft, SOURCE_CONTROL_WRITE_SCOPE, allowed_review_verdicts, apply_label_change, can_edit_change_request, description_draft_in_force, label_change_input, patch_detail,
    settle_description_save, start_description_save, thread_resolution_input, update_body_input, update_maintainer_edits_input, update_title_input,
};
use t3_core::pull_requests::reactions::set_reaction_input;
use t3_core::pull_requests::review::{
    PendingReviewComment, ReviewDraft, post_comment_input, reply_to_thread_input, review_verdict_sent_label, submit_review_input, thread_comments_input, update_comment_input,
};
use t3_core::pull_requests::reviewers::{host_reviewer_query, request_reviewers_input, reviewer_candidates_input};
use t3_core::pull_requests::tasks::set_task_checked;
use t3_core::pull_requests::wire::{
    Action, Activity, CommentKind, Detail, LabelCandidates, ListEntry, MergeMethod, PrState, ReactionContent, ReviewVerdict, ReviewerCandidate, ReviewerCandidates, Stack, Summary,
    ThreadCommentsPage, decode,
};
use t3_core::pull_requests::{PrRef, action_failed_label, action_input, action_needs_host_refresh, invalidate_input, method, stack_merge};

use super::code::CodeTab;
use super::controls::{BadgeKind, Button, Size, Variant, badge, diff_stat, state_glyph};
use super::guide::GuideTab;
use super::interactions;
use super::mutations::{self, MergePicks, Mutations};
use super::page::{PullRequestsPage, Refreshes};
use super::{read, settle};
use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::icons::Icon;
use crate::right_panel::{segment, segmented};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::markdown::MarkdownStore;

type ViewAction = Box<dyn Fn(&mut DetailView, &mut Context<DetailView>)>;

/// How long a first click stays armed (`useInlineActionConfirm`).
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);
/// The confirmation a menu item arms, shown beside the primary control.
const INLINE_CONFIRM: &str = "pr-confirm-inline";
/// How many pull requests' last reads are kept to paint from.
const HELD_LIMIT: usize = 32;

/// Key isolating held reads per environment, project/worktree, and pull request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HeldKey {
    environment: String,
    project_id: String,
    pr: String,
}

fn held_environment(state: &AppState) -> Option<&str> {
    state.environment().or(state.environments.active.as_deref())
}

impl HeldKey {
    fn of(state: &AppState, reference: &PrRef) -> Option<Self> {
        let environment = held_environment(state)?.to_owned();
        Some(HeldKey { environment, project_id: reference.project_id.clone(), pr: reference.key() })
    }
}

/// The last detail and activity read for each recently opened pull request, so reopening one
/// paints at once while it is read again (`pingdotgg/t3code#27798`). Bounded, least recent out.
#[derive(Default)]
struct Held(VecDeque<(HeldKey, Option<Detail>, Option<Activity>)>);

impl Global for Held {}

fn held(key: &HeldKey, cx: &App) -> (Option<Detail>, Option<Activity>) {
    cx.try_global::<Held>().and_then(|held| held.0.iter().find(|(held, ..)| held == key)).map(|(_, detail, activity)| (detail.clone(), activity.clone())).unwrap_or_default()
}

fn hold(key: HeldKey, detail: Option<&Detail>, activity: Option<&Activity>, cx: &mut App) {
    let held = cx.default_global::<Held>();
    let (_, kept_detail, kept_activity) = held.0.iter().position(|(held, ..)| *held == key).and_then(|index| held.0.remove(index)).unwrap_or_default();
    held.0.push_back((key, detail.cloned().or(kept_detail), activity.cloned().or(kept_activity)));
    while held.0.len() > HELD_LIMIT {
        held.0.pop_front();
    }
}

/// Seeds `Held` from a hover-intent prefetch (`usePullRequestIntentPrefetch`), scoped to the
/// active environment so clicking the row paints immediately.
pub(super) fn prefetch_into_held(state: &Entity<AppState>, reference: &PrRef, detail: Option<&Detail>, activity: Option<&Activity>, cx: &mut App) {
    let key = HeldKey::of(state.read(cx), reference);
    if let Some(key) = key {
        hold(key, detail, activity, cx);
    }
}

pub(super) fn has_held_detail(state: &AppState, reference: &PrRef, cx: &App) -> bool {
    HeldKey::of(state, reference).is_some_and(|key| held(&key, cx).0.is_some())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Summary,
    Guide,
    Timeline,
    Code,
}

pub(super) struct CommentEditState {
    pub comment_id: String,
    pub kind: CommentKind,
    pub input: Entity<TextareaState>,
    pub saving: bool,
    _sub: Subscription,
}

#[derive(Default)]
struct PendingReactionOverlay {
    signature: String,
    values: HashMap<ReactionContent, bool>,
}

pub(super) type CloseHandler = Rc<dyn Fn(&mut App)>;
pub(super) type SelectPrHandler = Rc<dyn Fn(PrRef, &mut App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Action(Action),
    Stack,
}

/// A first click on `element`; only a second click on the same element runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Armed {
    confirm: Confirm,
    element: &'static str,
}

pub struct DetailView {
    pub(super) state: Entity<AppState>,
    page: WeakEntity<PullRequestsPage>,
    on_close: Option<CloseHandler>,
    on_select_pr: Option<SelectPrHandler>,
    pub(super) reference: PrRef,
    entry: Option<ListEntry>,
    pub(super) detail: Option<Detail>,
    pub(super) activity: Option<Activity>,
    pub(super) error: Option<String>,
    pub(super) activity_error: Option<String>,
    pub(super) checks_open: bool,
    tab: Tab,
    /// This pull request's own merge method pick (`mergeMethodSelection`).
    merge_method: Option<MergeMethod>,
    /// `pullRequests.stack` where the host runs stack actions or the PR belongs to a stack.
    stack: Option<Result<Option<Stack>, String>>,
    stack_pending: bool,
    refreshing: bool,
    armed: Option<Armed>,
    /// Inline title editor (`PullRequestTitleEditor`).
    title_editing: bool,
    title_saving: bool,
    title_input: Option<Entity<InputState>>,
    _title_sub: Option<Subscription>,
    /// Inline description editor & task-checkbox draft.
    pub(super) description_editing: bool,
    description_input: Option<Entity<TextareaState>>,
    description_draft: Option<DescriptionDraft>,
    pub(super) description_saves: u32,
    pub(super) maintainer_edits_saving: bool,
    /// `pullRequests.labelCandidates`, read once the label picker can show.
    pub(super) label_candidates: Option<Result<LabelCandidates, String>>,
    pub(super) label_pending: bool,
    /// `PullRequestReviewerPicker` state.
    pub(super) reviewer_picker_open: bool,
    pub(super) reviewer_candidates: Option<Result<ReviewerCandidates, String>>,
    pub(super) reviewer_query: String,
    pub(super) reviewer_debounced_query: String,
    pub(super) reviewer_search_result: Option<Result<ReviewerCandidates, String>>,
    pub(super) reviewer_search_pending: bool,
    pub(super) reviewer_request_pending: Option<String>,
    reviewer_search_input: Option<Entity<InputState>>,
    _reviewer_sub: Option<Subscription>,
    _reviewer_debounce: Option<Task<()>>,
    _reviewers: Option<Task<()>>,
    _reviewer_search: Option<Task<()>>,
    /// Review submission form & inline pending review comments (`PullRequestReviewForm`).
    pub(super) review_form_open: bool,
    pub(super) review_draft: ReviewDraft,
    pub(super) review_submitting: bool,
    review_summary_input: Option<Entity<TextareaState>>,
    /// Top-level comment composer (`PullRequestCommentForm`).
    comment_input: Option<Entity<TextareaState>>,
    pub(super) comment_submitting: bool,
    pub(super) comment_close_armed: bool,
    _comment_disarm: Option<Task<()>>,
    /// Inline comment editing (`PullRequestCommentBody`).
    pub(super) editing_comment: Option<CommentEditState>,
    /// Inline thread reply composer (`pullRequests.replyToThread`).
    pub(super) replying_thread: Option<String>,
    reply_input: Option<Entity<TextareaState>>,
    pub(super) reply_submitting: bool,
    pub(super) loading_more_replies: HashSet<String>,
    /// Optimistic reactions overlay keyed by subject_id (or `"__pr__"` for the PR description).
    pending_reactions: HashMap<String, PendingReactionOverlay>,
    /// Inline thread-link picker (`PullRequestThreadLinks`).
    pub(super) thread_picker_open: bool,
    pub(super) thread_link_pending: bool,
    thread_picker_input: Option<Entity<InputState>>,
    _thread_picker_sub: Option<Subscription>,
    /// Threads whose resolution is being written.
    pub(super) resolving: HashSet<String>,
    /// Where a link points, still to reach once its tab holds what it names.
    reveal: Option<Anchor>,
    pub(super) summary_scroll: ScrollHandle,
    /// Environment this detail view belongs to, so cross-environment switches cannot pollute `Held`.
    environment: Option<String>,
    /// Threads linking this pull request, kept while re-read so the count does not flicker.
    linked_threads: Option<Vec<LinkedThread>>,
    linked_connection: Option<u64>,
    _linked_signal: Option<Refreshes>,
    _linked_read: Option<Task<()>>,
    /// Opens the description's and comments' media full size.
    viewer: Entity<crate::media::MediaViewer>,
    pub(super) markdown: MarkdownStore,
    pub(super) timeline: Vec<t3_core::pull_requests::detail::TimelineEvent>,
    pub(super) timeline_list: gpui::ListState,
    guide: Option<Entity<GuideTab>>,
    code: Option<Entity<CodeTab>>,
    _detail: Option<Task<()>>,
    _activity: Option<Task<()>>,
    _stack: Option<Task<()>>,
    _labels: Option<Task<()>>,
    _disarm: Option<Task<()>>,
    _escape: Option<Subscription>,
    _activation: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl DetailView {
    pub fn new(state: Entity<AppState>, page: WeakEntity<PullRequestsPage>, reference: PrRef, entry: Option<ListEntry>, cx: &mut Context<Self>) -> Self {
        Self::new_with_hooks(state, page, None, None, reference, entry, cx)
    }

    /// Creates a `DetailView` embedded inside another surface (such as `ThreadPullRequestsPanel`).
    pub(super) fn new_embedded(
        state: Entity<AppState>,
        reference: PrRef,
        entry: Option<ListEntry>,
        on_close: CloseHandler,
        on_select_pr: SelectPrHandler,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_hooks(state, WeakEntity::new_invalid(), Some(on_close), Some(on_select_pr), reference, entry, cx)
    }

    fn new_with_hooks(
        state: Entity<AppState>,
        page: WeakEntity<PullRequestsPage>,
        on_close: Option<CloseHandler>,
        on_select_pr: Option<SelectPrHandler>,
        reference: PrRef,
        entry: Option<ListEntry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
                if matches!(event, AppEvent::Selection | AppEvent::Phase) {
                    this.disarm(cx);
                }
                if *event == AppEvent::Phase {
                    this.sync_linked_threads(cx);
                }
            }),
            cx.observe_global::<Mutations>(|_, cx| cx.notify()),
        ];
        let environment = held_environment(state.read(cx)).map(str::to_owned);
        let (detail, activity) = HeldKey::of(state.read(cx), &reference).map(|key| held(&key, cx)).unwrap_or_default();
        crate::media::cache(&state, cx);
        let viewer = cx.new(crate::media::MediaViewer::new);
        let mut view = DetailView {
            state,
            page,
            on_close,
            on_select_pr,
            reference,
            entry,
            detail,
            activity,
            error: None,
            activity_error: None,
            checks_open: false,
            tab: Tab::Summary,
            merge_method: None,
            stack: None,
            stack_pending: false,
            refreshing: false,
            armed: None,
            title_editing: false,
            title_saving: false,
            title_input: None,
            _title_sub: None,
            description_editing: false,
            description_input: None,
            description_draft: None,
            description_saves: 0,
            maintainer_edits_saving: false,
            label_candidates: None,
            label_pending: false,
            reviewer_picker_open: false,
            reviewer_candidates: None,
            reviewer_query: String::new(),
            reviewer_debounced_query: String::new(),
            reviewer_search_result: None,
            reviewer_search_pending: false,
            reviewer_request_pending: None,
            reviewer_search_input: None,
            _reviewer_sub: None,
            _reviewer_debounce: None,
            _reviewers: None,
            _reviewer_search: None,
            review_form_open: false,
            review_draft: ReviewDraft::default(),
            review_submitting: false,
            review_summary_input: None,
            comment_input: None,
            comment_submitting: false,
            comment_close_armed: false,
            _comment_disarm: None,
            editing_comment: None,
            replying_thread: None,
            reply_input: None,
            reply_submitting: false,
            loading_more_replies: HashSet::new(),
            pending_reactions: HashMap::new(),
            thread_picker_open: false,
            thread_link_pending: false,
            thread_picker_input: None,
            _thread_picker_sub: None,
            resolving: HashSet::new(),
            reveal: None,
            summary_scroll: ScrollHandle::new(),
            environment,
            linked_threads: None,
            linked_connection: None,
            _linked_signal: None,
            _linked_read: None,
            viewer,
            markdown: MarkdownStore::new(cx.entity_id()),
            timeline: Vec::new(),
            timeline_list: gpui::ListState::new(0, gpui::ListAlignment::Top, px(600.)),
            guide: None,
            code: None,
            _detail: None,
            _activity: None,
            _stack: None,
            _labels: None,
            _disarm: None,
            _escape: None,
            _activation: None,
            _subscriptions: subscriptions,
        };
        view.rebuild_timeline();
        view.refresh(cx);
        view.sync_linked_threads(cx);
        view
    }

    /// Keeps the linked-threads subscription bound to the active connection: tears it down on
    /// disconnect or connection swap, and starts it again on reconnect.
    fn sync_linked_threads(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connected = state.client_handle().is_some() && matches!(state.phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected));
        let connection = state.connection_id();
        if !connected || connection != self.linked_connection {
            self._linked_signal = None;
            self._linked_read = None;
            self.linked_connection = None;
        }
        if connected && self._linked_signal.is_none() {
            self.linked_connection = connection;
            self.follow_linked_threads(cx);
        }
    }

    /// Re-reads the linked threads whenever a link changes
    /// (`pullRequests.subscribeLinkedThreadRefreshes`), or every ten seconds from a server that
    /// predates the signal. Dropping the view or disconnecting unsubscribes.
    fn follow_linked_threads(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else { return };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let net = state.runtime().spawn(async move {
            if tx.send(()).is_err() {
                return;
            }
            let mut delay = Duration::from_secs(1);
            loop {
                match client.stream(SUBSCRIBE_LINKED_THREAD_REFRESHES, serde_json::json!({})).await {
                    Ok(mut stream) => {
                        while let Some(item) = stream.receiver.recv().await {
                            match item {
                                Ok(_) => {
                                    delay = Duration::from_secs(1);
                                    if tx.send(()).is_err() {
                                        return;
                                    }
                                }
                                Err(t3_client::session::RpcError::Failure(failure)) if failure.tag.as_deref() == Some("EnvironmentAuthorizationError") => return,
                                Err(t3_client::session::RpcError::Closed) => return,
                                Err(_) => break,
                            }
                        }
                    }
                    Err(t3_client::session::RpcError::Failure(failure)) if failure.tag.as_deref() == Some("EnvironmentAuthorizationError") => return,
                    Err(t3_client::session::RpcError::Closed) => return,
                    Err(t3_client::session::RpcError::Failure(_)) => {
                        // Older servers reject `subscribeLinkedThreadRefreshes`; fall back to 10s polling.
                        loop {
                            tokio::time::sleep(Duration::from_secs(LINKED_THREADS_POLL_SECONDS)).await;
                            if tx.send(()).is_err() {
                                return;
                            }
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        });
        let ui = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                while rx.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.read_linked_threads(cx)).is_err() {
                    return;
                }
            }
        });
        self._linked_signal = Some(Refreshes { net, _ui: ui });
    }

    fn read_linked_threads(&mut self, cx: &mut Context<Self>) {
        let url = self.detail.as_ref().map(|detail| detail.url.clone()).or(self.entry.as_ref().map(|entry| entry.url.clone())).unwrap_or_default();
        let task = read(self.state.read(cx), LINKED_THREADS, linked_threads_input(&self.reference, &url), decode::<LinkedThreads>);
        self._linked_read = Some(cx.spawn(async move |this, cx| {
            let Ok(linked) = settle(task).await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.linked_threads.as_ref() != Some(&linked.threads) {
                    this.linked_threads = Some(linked.threads);
                    cx.notify();
                }
            });
        }));
    }

    /// Linked threads filtered through the environment's hidden-tag filter (`visibleLinkedThreads`).
    pub(crate) fn visible_linked_threads(&self, cx: &App) -> Vec<LinkedThread> {
        let Some(rows) = &self.linked_threads else { return Vec::new() };
        let state = self.state.read(cx);
        let loaded_ids: HashSet<&str> = state
            .shell
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.threads.iter().map(|t| t.id.as_str()).collect())
            .unwrap_or_default();
        let hiding_anywhere = state.hiding_thread_tags() || !state.sidebar.hidden_thread_ids.is_empty();
        visible_linked_threads(rows, &loaded_ids, &state.sidebar.hidden_thread_ids, hiding_anywhere)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The header's "Linked from N threads" (`PullRequestThreadLinks`'s count): clicking opens
    /// the command palette pre-seeded with the PR URL and visible linked threads.
    fn render_linked_threads(&self, cx: &App) -> Option<AnyElement> {
        let threads = self.visible_linked_threads(cx);
        if threads.is_empty() {
            return None;
        }
        let label = format!("{}. Search in the command palette.", linked_threads_label(threads.len()));
        let url = self
            .detail
            .as_ref()
            .map(|d| d.url.clone())
            .or_else(|| self.entry.as_ref().map(|e| e.url.clone()))
            .unwrap_or_else(|| format!("#{}", self.reference.number));
        Some(
            Button::new("pr-linked-threads", Variant::GhostMuted, Size::Xs)
                .icon(Icon::MessageSquare)
                .label(threads.len().to_string())
                .tooltip(label)
                .on_click(move |_, window, cx| {
                    cx.set_global(crate::palette::PendingLinkedThreadSearch(Some(crate::palette::LinkedThreadSearch {
                        query: url.clone(),
                        threads: threads.clone(),
                    })));
                    window.dispatch_action(Box::new(crate::keymap::ToggleCommandPalette), cx);
                })
                .into_any_element(),
        )
    }

    /// Media in the description and comments: a private host serves it only with a credential,
    /// so it is read through the server for the checkout, naming the pull request's own
    /// repository, which can differ from the checkout's (`pingdotgg/t3code#21631`).
    pub(super) fn media_scope(&self, detail: &Detail, cx: &Context<Self>) -> crate::media::MediaScope {
        crate::media::MediaScope {
            owner: cx.entity_id(),
            thread_id: None,
            workspace_root: Some(detail.workspace_root.clone()).filter(|root| !root.trim().is_empty()),
            workspace_mutation_id: None,
            source_control_media: true,
            source_control_media_repository: Some(detail.repository.clone()),
            viewer: self.viewer.downgrade(),
        }
    }

    fn hold(&self, cx: &mut App) {
        let Some(key) = HeldKey::of(self.state.read(cx), &self.reference) else { return };
        if self.environment.as_deref() != Some(key.environment.as_str()) {
            return;
        }
        hold(key, self.detail.as_ref(), self.activity.as_ref(), cx);
    }

    /// Re-reads the detail, its activity and stack membership, and the patch if it was read.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let detail = read(state, method::DETAIL, self.reference.payload(), decode::<Detail>);
        let activity = read(state, method::ACTIVITY, self.reference.payload(), decode::<Activity>);
        self._detail = Some(cx.spawn(async move |this, cx| {
            let result = settle(detail).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(detail) => {
                        let first_detail = this.detail.is_none();
                        this.detail = Some(detail);
                        this.error = None;
                        this.hold(cx);
                        this.refresh_stack(cx);
                        if first_detail && this.entry.is_none() {
                            this.read_linked_threads(cx);
                        }
                        if let Some(code) = this.code.clone() {
                            let detail = this.detail.clone();
                            let activity = this.activity.clone();
                            let can_write = this.can_write_source_control(cx);
                            code.update(cx, |code, cx| code.sync_state(detail.as_ref(), activity.as_ref(), can_write, cx));
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                this.rebuild_timeline();
                this.try_reveal();
                cx.notify();
            });
        }));
        self._activity = Some(cx.spawn(async move |this, cx| {
            let result = settle(activity).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(activity) => {
                        this.activity = Some(activity);
                        this.activity_error = None;
                        this.hold(cx);
                        if let Some(code) = this.code.clone() {
                            let detail = this.detail.clone();
                            let activity = this.activity.clone();
                            let can_write = this.can_write_source_control(cx);
                            code.update(cx, |code, cx| code.sync_state(detail.as_ref(), activity.as_ref(), can_write, cx));
                        }
                    }
                    Err(error) => this.activity_error = Some(error),
                }
                this.rebuild_timeline();
                this.try_reveal();
                cx.notify();
            });
        }));
        if let Some(code) = &self.code {
            code.update(cx, |code, cx| code.reload(cx));
        }
        if let Some(guide) = &self.guide {
            guide.update(cx, |guide, cx| guide.reload(cx));
        }
    }

    /// "Refresh from host": this pull request's host reads are dropped
    /// (`pullRequests.invalidate({ reference })`), then everything re-read.
    pub(super) fn refresh_from_host(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::INVALIDATE, invalidate_input(Some(&self.reference)), |_| Ok(()));
        self.refreshing = true;
        self._labels = None;
        self.label_candidates = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let _ = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Stack membership, read whenever the host supports stacks (for `PullRequestStackPopover`
    /// and stack-scoped merges). The last answer stands while re-read.
    fn refresh_stack(&mut self, cx: &mut Context<Self>) {
        let supported = self.detail.as_ref().is_some_and(|detail| detail.capabilities.stacks == Some(true)) || self.entry.as_ref().is_some_and(|entry| entry.stack.is_some());
        if !supported {
            self._stack = None;
            return;
        }
        let task = read(self.state.read(cx), method::STACK, self.reference.payload(), decode::<Option<Stack>>);
        self._stack = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.stack = Some(result);
                cx.notify();
            });
        }));
    }

    fn retry_stack(&mut self, cx: &mut Context<Self>) {
        self.stack = None;
        self.refresh_stack(cx);
        cx.notify();
    }

    /// `pullRequests.checks` for the Checks section, when the detail's are out of date.
    pub(super) fn refresh_checks(&mut self, cx: &mut Context<Self>) {
        let task = read(self.state.read(cx), method::CHECKS, self.reference.payload(), t3_core::pull_requests::wire::decode_checks);
        cx.spawn(async move |this, cx| {
            let Ok(Some(checks)) = settle(task).await else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(detail) = this.detail.as_mut() {
                    detail.checks = checks.checks;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn term(&self) -> &'static str {
        change_request_term(self.provider())
    }

    fn provider(&self) -> &str {
        self.detail.as_ref().map(|d| d.provider.as_str()).or(self.entry.as_ref().map(|e| e.provider.as_str())).unwrap_or("github")
    }

    fn gate(&self) -> MergeGate {
        self.detail.as_ref().map_or(MergeGate { single: false, stack_owns: false, blocked: None }, |detail| merge_gate(detail, self.stack.as_ref()))
    }

    fn picks(&self, cx: &gpui::App) -> MergePicks {
        self.page
            .upgrade()
            .map(|page| page.read(cx).merge_picks(&self.reference.project_id, self.merge_method, cx))
            .unwrap_or(MergePicks { current: self.merge_method, ..Default::default() })
    }

    fn selected_merge_method(&self, cx: &gpui::App) -> Option<MergeMethod> {
        let detail = self.detail.as_ref()?;
        let allowed = detail.allowed_merge_methods();
        let picks = self.picks(cx);
        (!allowed.is_empty()).then(|| resolve_merge_method(&allowed, picks.current, picks.project_default, picks.last))
    }

    /// A first click arms; a second on the same control within the window runs it
    /// (`useInlineActionConfirm`). Escape, a press anywhere else, the window losing focus, or a
    /// change of view disarms it.
    fn confirm(&mut self, confirm: Confirm, element: &'static str, cx: &mut Context<Self>) {
        let armed = Armed { confirm, element };
        if self.armed == Some(armed) {
            self.disarm(cx);
            match confirm {
                Confirm::Action(action) => self.perform(action, cx),
                Confirm::Stack => self.merge_stack(cx),
            }
            return;
        }
        self.armed = Some(armed);
        self._disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONFIRM_WINDOW).await;
            let _ = this.update(cx, |this, cx| this.disarm(cx));
        }));
        let this = cx.entity().downgrade();
        self._escape = Some(cx.intercept_keystrokes(move |event, _, cx| {
            if event.keystroke.key == "escape"
                && let Some(this) = this.upgrade()
            {
                this.update(cx, |this, cx| this.disarm(cx));
                cx.stop_propagation();
            }
        }));
        cx.notify();
    }

    fn disarm(&mut self, cx: &mut Context<Self>) {
        self._disarm = None;
        self._escape = None;
        if self.armed.take().is_some() {
            cx.notify();
        }
    }

    fn armed_on(&self, element: &'static str) -> Option<Confirm> {
        self.armed.filter(|armed| armed.element == element).map(|armed| armed.confirm)
    }

    fn perform(&mut self, action: Action, cx: &mut Context<Self>) {
        if !mutations::begin(&self.reference, action, cx) {
            return;
        }
        let task = if action == Action::Merge {
            let picks = self.picks(cx);
            mutations::merge(self.state.read(cx), self.reference.clone(), picks, cx)
        } else {
            let method = self.selected_merge_method(cx);
            mutations::run_action(self.state.read(cx), action_input(&self.reference, action, method), cx)
        };
        let term = self.term();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                mutations::finish(&this.reference, cx);
                if let Err(error) = &result {
                    let toast = Toast::new(ToastKind::Error, action_failed_label(action, term)).description(error.clone());
                    this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    return;
                }
                let reply = result.ok().flatten();
                let reread = this.apply_reply(reply.as_ref(), cx);
                let reference = this.reference.clone();
                if let Some(page) = this.page.upgrade() {
                    page.update(cx, |page, cx| page.acted(&reference, action, reply.as_ref(), cx));
                }
                if action_needs_host_refresh(action) {
                    this.refresh_from_host(cx);
                } else if reread {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// A stack-scoped merge of this layer and the unmerged ones below it, with the heads the
    /// host must still see (`PullRequestStackMenu`'s `run`).
    fn merge_stack(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(Some(stack))) = &self.stack else { return };
        let Some(method) = self.selected_merge_method(cx) else { return };
        let Some(merge) = stack_merge(stack, &self.reference, method) else { return };
        if !mutations::begin(&self.reference, Action::Merge, cx) {
            return;
        }
        self.stack_pending = true;
        let task = mutations::run_action(self.state.read(cx), merge.input, cx);
        let host = host_label(self.provider());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                mutations::finish(&this.reference, cx);
                this.stack_pending = false;
                let toast = match &result {
                    Ok(_) => Toast::new(ToastKind::Success, "Stack merge request completed").description(format!("{host} merged the stack or added it to its merge queue.")),
                    Err(error) => Toast::new(ToastKind::Error, "Could not merge the stack").description(error.clone()),
                };
                this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                this.refresh(cx);
                if let Some(page) = this.page.upgrade() {
                    page.update(cx, |page, cx| page.refetch(cx));
                }
            });
        })
        .detach();
    }

    /// An action's reply shown at once (the detail's `patch`). True when the detail must still be
    /// read: the server sent no reply, or the reply changes what it lacks.
    pub(super) fn apply_reply(&mut self, reply: Option<&Summary>, cx: &mut Context<Self>) -> bool {
        let (Some(reply), Some(detail)) = (reply, self.detail.as_ref()) else { return true };
        let Some((next, reread)) = patch_detail(detail, reply) else { return false };
        self.detail = Some(next);
        self.hold(cx);
        self.rebuild_timeline();
        cx.notify();
        reread
    }

    /// Whether this connection holds `source-control:write` (`usePullRequestDetail`'s
    /// `canWriteSourceControl`); unknown grants leave the server to decide.
    pub(super) fn can_write_source_control(&self, cx: &App) -> bool {
        self.state.read(cx).client_handle().and_then(|handle| handle.session_state().borrow().clone()).is_none_or(|session| session.grants(SOURCE_CONTROL_WRITE_SCOPE))
    }

    /// The description shown: this client's draft while it is in force, else the host's.
    pub(super) fn description(&mut self) -> Option<String> {
        let detail = self.detail.as_ref()?;
        self.description_draft = description_draft_in_force(self.description_draft.take(), &detail.url, detail.observed_at_ms());
        Some(self.description_draft.as_ref().and_then(|draft| draft.body.clone()).unwrap_or_else(|| detail.body.clone()))
    }

    /// The host lets this viewer rewrite the description, and this connection may write
    /// (`pullRequestEnvironment.update.permissionAtom`).
    pub(super) fn can_edit_description(&self, cx: &App) -> bool {
        self.can_write_source_control(cx) && self.detail.as_ref().is_some_and(can_edit_change_request)
    }

    /// Whether there is an active thread composer to receive a PR comment (`pingdotgg/t3code#30579`).
    pub(super) fn has_chat_target(&self, cx: &App) -> bool {
        self.state.read(cx).selected_thread.is_some()
    }

    /// Brings a comment or review/overview thread into the active thread's composer alongside the
    /// pull request context chip (`addCommentToChat` in `PullRequestDetailPanel.tsx`).
    pub(super) fn add_comment_to_chat(&mut self, thread_id: Option<&str>, comment_id: &str, cx: &mut Context<Self>) {
        let (Some(detail), Some(activity)) = (self.detail.as_ref(), self.activity.as_ref()) else { return };
        let Some(target_thread) = self.state.read(cx).selected_thread.clone() else { return };
        let thread = thread_id
            .and_then(|id| activity.review_threads.iter().chain(&activity.overview_threads).find(|candidate| candidate.id == id))
            .or_else(|| {
                activity
                    .review_threads
                    .iter()
                    .chain(&activity.overview_threads)
                    .find(|candidate| candidate.comments.iter().any(|comment| comment.id == comment_id))
            });
        let comment_chip = match thread {
            Some(thread) => build_comment_context(detail, ChatSubject::Thread(thread)),
            None => activity.comments.iter().find(|comment| comment.id == comment_id).and_then(|comment| build_comment_context(detail, ChatSubject::Comment(comment))),
        };
        let Some(comment_chip) = comment_chip else { return };
        let pr_chip = build_pull_request_context(detail);
        let has_reference = self.state.read(cx).review_comments.get(&target_thread).is_some_and(|held| held.iter().any(|existing| existing.id == pr_chip.id));
        if has_reference {
            self.state.update(cx, |state, cx| {
                state.add_review_comment(&target_thread, comment_chip, cx);
                state.show_toast(Toast::new(ToastKind::Success, "Added to the composer"), cx);
            });
        } else {
            let state = self.state.clone();
            state.update(cx, |state, cx| state.add_review_comment(&target_thread, pr_chip, cx));
            // Defer the second chip so `Composer` consumes the PR chip before the slot is reused.
            cx.defer(move |cx| {
                state.update(cx, |state, cx| {
                    state.add_review_comment(&target_thread, comment_chip, cx);
                    state.show_toast(Toast::new(ToastKind::Success, "Added to the composer"), cx);
                });
            });
        }
    }

    /// A task checkbox: its marker rewritten in the latest description, then saved.
    pub(super) fn toggle_task(&mut self, marker: usize, checked: bool, cx: &mut Context<Self>) {
        let Some(description) = self.description() else { return };
        let body = set_task_checked(&description, marker, checked);
        if body != description {
            self.save_description(body, cx);
        }
    }

    /// `saveDescription`: checkbox ticks share one draft, so each starts from the latest body.
    /// Readers refresh once the last save settles.
    fn save_description(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.as_ref() else { return };
        let key = detail.url.clone();
        self.description_draft = Some(start_description_save(self.description_draft.take(), &key, detail.observed_at_ms(), body.clone()));
        self.description_saves += 1;
        let task = mutations::write(self.state.read(cx), editing::method::UPDATE, update_body_input(&self.reference, &body), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = settle(task).await.is_ok();
            let _ = this.update(cx, |this, cx| {
                this.description_saves = this.description_saves.saturating_sub(1);
                this.description_draft = settle_description_save(this.description_draft.take(), &key, saved);
                if !saved {
                    this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not save the description"), cx));
                }
                if this.description_saves == 0 {
                    this.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `PullRequestResolveThreadButton`: resolves or reopens a thread, review or overview.
    pub(super) fn set_thread_resolution(&mut self, thread_id: String, resolved: bool, cx: &mut Context<Self>) {
        if !self.resolving.insert(thread_id.clone()) {
            return;
        }
        let task = mutations::write(self.state.read(cx), editing::method::SET_THREAD_RESOLUTION, thread_resolution_input(&self.reference, &thread_id, resolved), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.resolving.remove(&thread_id);
                match result {
                    Ok(()) => this.refresh(cx),
                    Err(_) => this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "The conversation could not be updated"), cx)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `labelCandidates`, read once for the label picker; clears the in-flight task on error so
    /// reopening the label picker retries cleanly.
    pub(super) fn load_label_candidates(&mut self, cx: &mut Context<Self>) {
        if self._labels.is_some() {
            return;
        }
        let task = read(self.state.read(cx), editing::method::LABEL_CANDIDATES, self.reference.payload(), decode::<LabelCandidates>);
        self._labels = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                if result.is_err() {
                    this._labels = None;
                }
                this.label_candidates = Some(result);
                cx.notify();
            });
        }));
    }

    pub(super) fn retry_label_candidates(&mut self, cx: &mut Context<Self>) {
        self._labels = None;
        self.label_candidates = None;
        self.load_label_candidates(cx);
        cx.notify();
    }

    /// `PullRequestLabelPicker`'s toggle: one label on or off, shown on the detail once written.
    pub(super) fn toggle_label(&mut self, name: String, applied: bool, cx: &mut Context<Self>) {
        if self.label_pending {
            return;
        }
        self.label_pending = true;
        let task = mutations::write(self.state.read(cx), editing::method::SET_LABELS, label_change_input(&self.reference, &name, !applied), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.label_pending = false;
                match result {
                    Ok(()) => {
                        let mut candidates = this.label_candidates.take();
                        let held: &mut [_] = match candidates.as_mut() {
                            Some(Ok(list)) => &mut list.candidates,
                            _ => &mut [],
                        };
                        if let Some(detail) = this.detail.as_mut() {
                            apply_label_change(&mut detail.labels, held, &name, !applied);
                        }
                        this.label_candidates = candidates;
                        this.hold(cx);
                    }
                    Err(error) => {
                        let title = if applied { format!("Could not take {name} off") } else { format!("Could not put {name} on") };
                        let description = if error.trim().is_empty() { "The host refused it. Check that you have triage access on this repository.".to_owned() } else { error };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(description), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Goes where a pull request link points (`linkedTab` and the tabs' reveals): its tab, then
    /// the checks, a commit or comment in the timeline, or a file and lines in the code.
    pub fn reveal(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(anchor) = parse_anchor(url) else { return };
        let tab = match anchor.tab() {
            LinkedTab::Summary => Tab::Summary,
            LinkedTab::Timeline => Tab::Timeline,
            LinkedTab::Code => Tab::Code,
        };
        self.set_tab(tab, cx);
        if tab == Tab::Code
            && let Some(code) = &self.code
        {
            let commit = link_commit(url);
            code.update(cx, |code, cx| code.set_commit(commit, cx));
        }
        match anchor {
            Anchor::Checks => {
                self.checks_open = true;
                self.reveal = Some(Anchor::Checks);
            }
            Anchor::Diff { digest, range } => {
                if let Some(code) = &self.code {
                    code.update(cx, |code, cx| code.reveal_diff(digest, range, cx));
                }
            }
            Anchor::Commits | Anchor::Timeline(_) | Anchor::Summary(_) => self.reveal = Some(anchor),
            Anchor::Code => self.reveal = None,
        }
        self.try_reveal();
        cx.notify();
    }

    /// Reaches a pending reveal once its tab holds the place; a place the loaded activity does
    /// not have is given up.
    fn try_reveal(&mut self) {
        match &self.reveal {
            Some(Anchor::Checks) if self.detail.is_some() => {
                // Summary child 3 is the Checks section heading.
                self.summary_scroll.scroll_to_top_of_item(3);
                self.reveal = None;
            }
            Some(Anchor::Summary(anchor)) if self.detail.is_some() => {
                // Summary child 0 is top metadata, child 1 is the Description section heading.
                self.summary_scroll.scroll_to_top_of_item(if anchor == "top" { 0 } else { 1 });
                self.reveal = None;
            }
            Some(Anchor::Commits) if self.activity.is_some() => {
                if let Some(index) = self.timeline.iter().position(|event| event.kind == EventKind::Commit) {
                    self.timeline_list.scroll_to_reveal_item(index);
                }
                self.reveal = None;
            }
            Some(Anchor::Timeline(anchor)) if self.activity.is_some() => {
                let resolved = self.activity.as_ref().map_or_else(|| anchor.clone(), |activity| resolve_review_anchor(anchor, &activity.comments, &activity.review_threads));
                let names = |event: &t3_core::pull_requests::detail::TimelineEvent| anchor_matches_url(&resolved, event.url.as_deref());
                let index = self.timeline.iter().position(|event| names(event) || event.thread.as_ref().is_some_and(|thread| thread.replies.iter().any(names)));
                if let Some(index) = index {
                    self.timeline_list.scroll_to_reveal_item(index);
                }
                self.reveal = None;
            }
            _ => {}
        }
    }

    fn select_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        self.merge_method = Some(method);
        if let Some(page) = self.page.upgrade() {
            page.update(cx, |page, _| page.remember_merge_method(method));
        }
        cx.notify();
    }

    pub(super) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.disarm(cx);
        self.tab = tab;
        if tab == Tab::Code && self.code.is_none() {
            let state = self.state.clone();
            let detail_view = cx.entity().downgrade();
            let reference = self.reference.clone();
            let detail = self.detail.clone();
            let activity = self.activity.clone();
            let can_write = self.can_write_source_control(cx);
            self.code = Some(cx.new(|cx| CodeTab::new(state, detail_view, reference, detail.as_ref(), activity.as_ref(), can_write, cx)));
        }
        if tab == Tab::Guide {
            if let Some(guide) = &self.guide {
                guide.update(cx, |guide, _| guide.request_focus());
            } else {
                let state = self.state.clone();
                let detail_view = cx.entity().downgrade();
                let reference = self.reference.clone();
                self.guide = Some(cx.new(|cx| GuideTab::new(state, detail_view, reference, cx)));
            }
        }
        cx.notify();
    }

    /// Jumps from the Guide tab straight to `path` in the Code tab (`onOpenFile`).
    pub(super) fn open_code_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.set_tab(Tab::Code, cx);
        if let Some(code) = &self.code {
            code.update(cx, |code, cx| code.reveal_file_path(path, cx));
        }
    }

    /// Navigates to another PR in the same stack (`PullRequestStackPopover`).
    pub(super) fn select_stack_layer(&mut self, number: u64, cx: &mut Context<Self>) {
        if number == self.reference.number {
            return;
        }
        let target = PrRef { number, ..self.reference.clone() };
        if let Some(handler) = &self.on_select_pr {
            handler(target, cx);
        } else if let Some(page) = self.page.upgrade() {
            page.update(cx, |page, cx| page.select(target, cx));
        }
    }

    fn close_self(&mut self, cx: &mut Context<Self>) {
        if let Some(handler) = &self.on_close {
            handler(cx);
        } else if let Some(page) = self.page.upgrade() {
            page.update(cx, |page, cx| page.close_detail(cx));
        }
    }

    // --- Title & Description editors (`PullRequestTitleEditor`, `PullRequestSummaryTab`) ---

    fn start_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.detail.as_ref().map(|d| d.title.clone()) else { return };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.save_title(cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.title_input = Some(input);
        self._title_sub = Some(sub);
        self.title_editing = true;
        cx.notify();
    }

    fn save_title(&mut self, cx: &mut Context<Self>) {
        if self.title_saving || !self.can_write_source_control(cx) {
            return;
        }
        let Some(input) = &self.title_input else { return };
        let next = input.read(cx).value().trim().to_owned();
        if next.is_empty() || self.detail.as_ref().is_some_and(|d| d.title == next) {
            self.title_editing = false;
            cx.notify();
            return;
        }
        self.title_saving = true;
        let task = mutations::write(self.state.read(cx), editing::method::UPDATE, update_title_input(&self.reference, &next), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.title_saving = false;
                match res {
                    Ok(()) => {
                        if let Some(detail) = this.detail.as_mut() {
                            detail.title = next;
                        }
                        this.title_editing = false;
                        this.hold(cx);
                        this.refresh(cx);
                    }
                    Err(err) => {
                        let desc = if err.trim().is_empty() { "The host refused the new title.".to_owned() } else { err };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "The title could not be saved").description(desc), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_description_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.description().unwrap_or_default();
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 14).default_value(body).placeholder("Describe this change..."));
        self.description_input = Some(input);
        self.description_editing = true;
        cx.notify();
    }

    pub(super) fn ensure_description_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.description_input {
            return input.clone();
        }
        let body = self.description().unwrap_or_default();
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 14).default_value(body).placeholder("Describe this change..."));
        self.description_input = Some(input.clone());
        input
    }

    pub(super) fn commit_description_edit(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.description_input else { return };
        let body = input.read(cx).value().to_string();
        self.description_editing = false;
        self.save_description(body, cx);
    }

    pub(super) fn toggle_maintainer_edits(&mut self, allow: bool, cx: &mut Context<Self>) {
        if self.maintainer_edits_saving || !self.can_write_source_control(cx) {
            return;
        }
        self.maintainer_edits_saving = true;
        let task = mutations::write(self.state.read(cx), editing::method::UPDATE, update_maintainer_edits_input(&self.reference, allow), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.maintainer_edits_saving = false;
                match res {
                    Ok(()) => {
                        if let Some(detail) = this.detail.as_mut() {
                            detail.maintainer_can_modify = Some(allow);
                        }
                        this.hold(cx);
                        this.refresh(cx);
                    }
                    Err(err) => {
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not update maintainer edits").description(err), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Reviewer picker (`PullRequestReviewerPicker`) ---

    pub(super) fn toggle_reviewer_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reviewer_picker_open = !self.reviewer_picker_open;
        if self.reviewer_picker_open {
            self.load_reviewer_candidates(cx);
            let input = self.ensure_reviewer_search_input(window, cx);
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn ensure_reviewer_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.reviewer_search_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search people with access"));
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                this.reviewer_query = query.clone();
                this._reviewer_debounce = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.reviewer_debounced_query = query.trim().to_owned();
                        this.maybe_search_reviewers(cx);
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        });
        self.reviewer_search_input = Some(input.clone());
        self._reviewer_sub = Some(sub);
        input
    }

    fn load_reviewer_candidates(&mut self, cx: &mut Context<Self>) {
        if self._reviewers.is_some() {
            return;
        }
        let task = read(self.state.read(cx), method::REVIEWER_CANDIDATES, reviewer_candidates_input(&self.reference, None), decode::<ReviewerCandidates>);
        self._reviewers = Some(cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                if res.is_err() {
                    this._reviewers = None;
                }
                this.reviewer_candidates = Some(res);
                this.maybe_search_reviewers(cx);
                cx.notify();
            });
        }));
    }

    pub(super) fn retry_reviewer_candidates(&mut self, cx: &mut Context<Self>) {
        self._reviewers = None;
        self.reviewer_candidates = None;
        self.load_reviewer_candidates(cx);
        cx.notify();
    }

    fn maybe_search_reviewers(&mut self, cx: &mut Context<Self>) {
        let searchable = self.detail.as_ref().and_then(|d| d.capabilities.reviewers.search_candidates) == Some(true);
        let loaded = self.reviewer_candidates.as_ref().and_then(|r| r.as_ref().ok());
        let Some(host_q) = host_reviewer_query(loaded, searchable, &self.reviewer_debounced_query).map(str::to_owned) else {
            self.reviewer_search_pending = false;
            return;
        };
        self.reviewer_search_pending = true;
        let task = read(self.state.read(cx), method::REVIEWER_CANDIDATES, reviewer_candidates_input(&self.reference, Some(&host_q)), decode::<ReviewerCandidates>);
        self._reviewer_search = Some(cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                if this.reviewer_debounced_query != host_q {
                    return;
                }
                this.reviewer_search_pending = false;
                this.reviewer_search_result = Some(res);
                cx.notify();
            });
        }));
    }

    pub(super) fn toggle_reviewer(&mut self, candidate: ReviewerCandidate, cx: &mut Context<Self>) {
        if self.reviewer_request_pending.is_some() || !self.can_write_source_control(cx) {
            return;
        }
        let next_requested = !candidate.is_requested;
        self.reviewer_request_pending = Some(candidate.id.clone());
        let payload = request_reviewers_input(&self.reference, &candidate.id, candidate.kind, next_requested);
        let task = mutations::write(self.state.read(cx), method::REQUEST_REVIEWERS, payload, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.reviewer_request_pending = None;
                match res {
                    Ok(()) => {
                        if let Some(Ok(list)) = this.reviewer_candidates.as_mut() {
                            for item in list.candidates.iter_mut().filter(|c| c.id == candidate.id && c.kind == candidate.kind) {
                                item.is_requested = next_requested;
                            }
                        }
                        if let Some(Ok(list)) = this.reviewer_search_result.as_mut() {
                            for item in list.candidates.iter_mut().filter(|c| c.id == candidate.id && c.kind == candidate.kind) {
                                item.is_requested = next_requested;
                            }
                        }
                        if let Some(detail) = this.detail.as_mut() {
                            if next_requested {
                                if !detail.reviewers.iter().any(|r| r.login.eq_ignore_ascii_case(&candidate.login)) {
                                    detail.reviewers.push(t3_core::pull_requests::wire::Actor {
                                        is_bot: candidate.is_bot,
                                        login: candidate.login.clone(),
                                        name: candidate.name.clone(),
                                        avatar_url: candidate.avatar_url.clone(),
                                    });
                                }
                            } else {
                                detail.reviewers.retain(|r| !r.login.eq_ignore_ascii_case(&candidate.login));
                            }
                        }
                        this.hold(cx);
                        let title = if next_requested {
                            format!("Review requested from {}", candidate.login)
                        } else {
                            format!("Review request to {} taken back", candidate.login)
                        };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Success, title), cx));
                        this.refresh(cx);
                    }
                    Err(err) => {
                        let title = if next_requested {
                            format!("Could not ask {} for a review", candidate.login)
                        } else {
                            format!("Could not take back the review request to {}", candidate.login)
                        };
                        let desc = if err.trim().is_empty() {
                            "The host refused it. Check that you have write access on this repository, and that they still have access to it.".to_owned()
                        } else {
                            err
                        };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(desc), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Review submission & inline comments (`PullRequestReviewForm`) ---

    pub(super) fn pending_review_comment_count(&self) -> usize {
        self.review_draft.comments.len()
    }

    pub(super) fn add_pending_review_comment(&mut self, comment: PendingReviewComment, cx: &mut Context<Self>) {
        self.review_draft.add_comment(comment);
        cx.notify();
    }

    pub(super) fn remove_pending_review_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.review_draft.remove_comment(id);
        cx.notify();
    }

    pub(super) fn open_review_form(&mut self, cx: &mut Context<Self>) {
        self.review_form_open = true;
        cx.notify();
    }

    pub(super) fn ensure_review_summary_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.review_summary_input {
            return input.clone();
        }
        let initial = self.review_draft.summary.clone();
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).default_value(initial).placeholder("Summarize your review (optional)"));
        let sub = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.review_summary_input = Some(input.clone());
        input
    }

    pub(super) fn submit_review(&mut self, verdict: ReviewVerdict, window: &mut Window, cx: &mut Context<Self>) {
        if self.review_submitting || !self.can_write_source_control(cx) {
            return;
        }
        let body = self.review_summary_input.as_ref().map(|i| i.read(cx).value().to_string()).unwrap_or_default();
        self.review_draft.summary = body.clone();
        let submitted_comments = self.review_draft.comments.clone();
        let submitted_ids: Vec<String> = submitted_comments.iter().map(|c| c.id.clone()).collect();
        let payload = submit_review_input(&self.reference, verdict, &body, &submitted_comments);
        let task = mutations::write(self.state.read(cx), method::SUBMIT_REVIEW, payload, cx);
        let term = self.term();
        self.review_submitting = true;
        let window_handle = window.window_handle();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.review_submitting = false;
                match res {
                    Ok(()) => {
                        this.review_draft.finish_submission(&submitted_ids, &body);
                        if this.review_draft.summary.is_empty()
                            && let Some(input) = this.review_summary_input.clone()
                        {
                            let _ = cx.update_window(window_handle, |_, window, cx| {
                                input.update(cx, |input, cx| input.set_value("", window, cx));
                            });
                        }
                        this.review_form_open = false;
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Success, review_verdict_sent_label(verdict, term)), cx));
                        this.refresh(cx);
                    }
                    Err(err) => {
                        let desc = if err.trim().is_empty() { "The host refused the review. Your draft is still here to try again.".to_owned() } else { err };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "The review could not be submitted").description(desc), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Comments, replies, comment edits, load-more replies ---

    pub(super) fn ensure_comment_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.comment_input {
            return input.clone();
        }
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("Leave a comment"));
        let sub = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.comment_input = Some(input.clone());
        input
    }

    pub(super) fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_submitting || !self.can_write_source_control(cx) {
            return;
        }
        let Some(input) = self.comment_input.clone() else { return };
        let trimmed = input.read(cx).value().trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        self.comment_submitting = true;
        let task = mutations::write(self.state.read(cx), method::COMMENT, post_comment_input(&self.reference, &trimmed), cx);
        let window_handle = window.window_handle();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.comment_submitting = false;
                match res {
                    Ok(()) => {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        });
                        this.refresh(cx);
                    }
                    Err(err) => {
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not post the comment").description(err), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn submit_comment_with_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if action == Action::Close && !self.comment_close_armed {
            self.comment_close_armed = true;
            self._comment_disarm = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CONFIRM_WINDOW).await;
                let _ = this.update(cx, |this, cx| {
                    this.comment_close_armed = false;
                    cx.notify();
                });
            }));
            cx.notify();
            return;
        }
        self.comment_close_armed = false;
        self._comment_disarm = None;
        if self.comment_submitting || !self.can_write_source_control(cx) {
            return;
        }
        let Some(input) = self.comment_input.clone() else { return };
        let trimmed = input.read(cx).value().trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        self.comment_submitting = true;
        let comment_task = mutations::write(self.state.read(cx), method::COMMENT, post_comment_input(&self.reference, &trimmed), cx);
        let action_task = mutations::run_action(self.state.read(cx), action_input(&self.reference, action, None), cx);
        let term = self.term();
        let window_handle = window.window_handle();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let comment_res = settle(comment_task).await;
            let action_res = if comment_res.is_ok() { settle(action_task).await } else { Ok(None) };
            let _ = this.update(cx, |this, cx| {
                this.comment_submitting = false;
                if let Err(err) = comment_res {
                    this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not post the comment").description(err), cx));
                    cx.notify();
                    return;
                }
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                });
                match action_res {
                    Ok(reply) => {
                        let reference = this.reference.clone();
                        this.apply_reply(reply.as_ref(), cx);
                        if let Some(page) = this.page.upgrade() {
                            page.update(cx, |page, cx| page.acted(&reference, action, reply.as_ref(), cx));
                        }
                        this.refresh(cx);
                    }
                    Err(err) => {
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, action_failed_label(action, term)).description(err), cx));
                        this.refresh(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_comment_edit(&mut self, comment_id: String, kind: CommentKind, body: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10).default_value(body));
        let sub = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self.editing_comment = Some(CommentEditState { comment_id, kind, input, saving: false, _sub: sub });
        cx.notify();
    }

    pub(super) fn commit_comment_edit(&mut self, cx: &mut Context<Self>) {
        if !self.can_write_source_control(cx) {
            return;
        }
        let Some(edit) = self.editing_comment.as_mut() else { return };
        if edit.saving {
            return;
        }
        let body = edit.input.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        edit.saving = true;
        let (comment_id, kind) = (edit.comment_id.clone(), edit.kind);
        let task = mutations::write(self.state.read(cx), method::UPDATE_COMMENT, update_comment_input(&self.reference, &comment_id, kind, &body), cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        this.editing_comment = None;
                        this.refresh(cx);
                    }
                    Err(err) => {
                        if let Some(edit) = this.editing_comment.as_mut() {
                            edit.saving = false;
                        }
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "The comment could not be saved").description(err), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_thread_reply(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.replying_thread = Some(thread_id);
        let input = self.ensure_reply_input(window, cx);
        input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    pub(super) fn ensure_reply_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.reply_input {
            return input.clone();
        }
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8).placeholder("Write a reply..."));
        let sub = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.reply_input = Some(input.clone());
        input
    }

    pub(super) fn submit_thread_reply(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.reply_submitting || !self.can_write_source_control(cx) {
            return;
        }
        let Some(input) = self.reply_input.clone() else { return };
        let body = input.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        self.reply_submitting = true;
        let task = mutations::write(self.state.read(cx), method::REPLY_TO_THREAD, reply_to_thread_input(&self.reference, &thread_id, &body), cx);
        let window_handle = window.window_handle();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.reply_submitting = false;
                match res {
                    Ok(()) => {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        });
                        this.replying_thread = None;
                        this.refresh(cx);
                    }
                    Err(err) => {
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not post the reply").description(err), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_more_thread_replies(&mut self, thread_id: String, cursor: String, cx: &mut Context<Self>) {
        if !self.loading_more_replies.insert(thread_id.clone()) {
            return;
        }
        let task = read(self.state.read(cx), method::THREAD_COMMENTS, thread_comments_input(&self.reference, &thread_id, &cursor), decode::<ThreadCommentsPage>);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                this.loading_more_replies.remove(&thread_id);
                if let Ok(page) = res
                    && let Some(activity) = this.activity.as_mut()
                {
                    for thread in activity.review_threads.iter_mut().chain(&mut activity.overview_threads) {
                        if thread.id != thread_id {
                            continue;
                        }
                        for comment in page.comments {
                            if !thread.comments.iter().any(|existing| existing.id == comment.id) {
                                thread.comments.push(comment);
                            }
                        }
                        thread.next_comments_cursor = page.next_cursor;
                        break;
                    }
                    this.hold(cx);
                    this.rebuild_timeline();
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Reactions (`PullRequestReactionBar`) ---

    pub(super) fn pending_reactions_for(&self, subject_key: &str, signature: &str) -> HashMap<ReactionContent, bool> {
        self.pending_reactions
            .get(subject_key)
            .filter(|overlay| overlay.signature == signature)
            .map(|overlay| overlay.values.clone())
            .unwrap_or_default()
    }

    pub(super) fn toggle_reaction(&mut self, subject_id: Option<String>, content: ReactionContent, reacted: bool, signature: String, cx: &mut Context<Self>) {
        if !self.can_write_source_control(cx) {
            return;
        }
        let subject_key = subject_id.clone().unwrap_or_else(|| "__pr__".to_owned());
        let entry = self.pending_reactions.entry(subject_key.clone()).or_default();
        if entry.signature != signature {
            entry.signature = signature.clone();
            entry.values.clear();
        }
        entry.values.insert(content, reacted);
        let payload = set_reaction_input(&self.reference, subject_id.as_deref(), content, reacted);
        let task = mutations::write(self.state.read(cx), method::SET_REACTION, payload, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = settle(task).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => this.refresh(cx),
                    Err(_) => {
                        if let Some(overlay) = this.pending_reactions.get_mut(&subject_key) {
                            overlay.values.remove(&content);
                        }
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "The reaction could not be saved"), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Inline thread-link picker (`PullRequestThreadLinks`) ---

    fn pr_url(&self) -> Option<String> {
        self.detail.as_ref().map(|d| d.url.clone()).or_else(|| self.entry.as_ref().map(|e| e.url.clone()))
    }

    fn is_linked_to_active_thread(&self, cx: &App) -> Option<(String, bool)> {
        let url = self.pr_url()?;
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone()?;
        let shell = state.thread_shell(&thread_id)?;
        Some((thread_id, is_linked_to_url(&shell.pull_requests, &url)))
    }

    pub(super) fn open_thread_picker(&mut self, cx: &mut Context<Self>) {
        self.thread_picker_open = true;
        cx.notify();
    }

    pub(super) fn ensure_thread_picker_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = &self.thread_picker_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads or projects..."));
        let sub = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.thread_picker_input = Some(input.clone());
        self._thread_picker_sub = Some(sub);
        input
    }

    pub(super) fn linkable_threads(&self, cx: &App) -> Vec<LinkableThread> {
        let url = self.pr_url().unwrap_or_default();
        let state = self.state.read(cx);
        let Some(snapshot) = &state.shell.snapshot else { return Vec::new() };
        let project_names: HashMap<&str, &str> = snapshot.projects.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
        snapshot
            .threads
            .iter()
            .map(|thread| LinkableThread {
                id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                project_title: project_names.get(thread.project_id.as_str()).copied().unwrap_or("").to_owned(),
                title: thread.title.clone(),
                updated_at: thread.updated_at.clone(),
                archived: thread.archived_at.is_some(),
                hidden: state.sidebar.hidden_thread_ids.contains(&thread.id),
                already_linked: is_linked_to_url(&thread.pull_requests, &url),
            })
            .collect()
    }

    pub(super) fn link_to_thread(&mut self, thread_id: String, remove: bool, cx: &mut Context<Self>) {
        if self.thread_link_pending {
            return;
        }
        let Some(url) = self.pr_url() else { return };
        let Some(link) = parse_change_request_url(&url) else { return };
        let command = if remove {
            let thread_link = t3_core::pull_requests::link::ThreadLink {
                host: link.host,
                repository: link.repository,
                number: link.number,
                url: link.url,
                source: "manual".into(),
                linked_at: String::new(),
                snapshot: None,
                stack: None,
                watch: None,
            };
            unlink_command(&uuid_v4(), &thread_id, &thread_link)
        } else {
            link_command(&uuid_v4(), &thread_id, &link)
        };
        self.thread_link_pending = true;
        let task = self.state.read(cx).rpc(DISPATCH_COMMAND, command);
        let term = self.term();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |this, cx| {
                this.thread_link_pending = false;
                match res {
                    Ok(_) => {
                        this.thread_picker_open = false;
                        this.read_linked_threads(cx);
                    }
                    Err(err) => {
                        let title = if remove { format!("Could not unlink the {term}") } else { format!("Could not link the {term}") };
                        this.state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, title).description(err.to_string()), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let title = self.detail.as_ref().map(|d| d.title.clone()).or(self.entry.as_ref().map(|e| e.title.clone())).unwrap_or_default();
        let (state, is_draft) = self.detail.as_ref().map(|d| (d.state, d.is_draft)).or(self.entry.as_ref().map(|e| (e.state, e.is_draft))).unwrap_or((PrState::Open, false));
        let kind = StateKind::of(state, is_draft);
        let (icon, color) = state_glyph(kind, cx);
        let url = self.detail.as_ref().map(|d| d.url.clone()).or(self.entry.as_ref().map(|e| e.url.clone()));
        let meta = self.detail.as_ref().map(|d| {
            let author = d.author.as_ref().map_or("ghost".to_owned(), |a| a.login.clone());
            (format!("{author} wants to merge into {} from {}", d.base_branch, d.head_branch), d.additions, d.deletions, d.changed_files)
        });
        let can_edit_title = self.can_edit_description(cx);
        let title_row: AnyElement = if self.title_editing && let Some(input) = &self.title_input {
            let saving = self.title_saving;
            let empty = input.read(cx).value().trim().is_empty();
            v_flex()
                .gap(px(6.))
                .child(Input::new(input).disabled(saving))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(Button::new("pr-title-cancel", Variant::Ghost, Size::Xs).label("Cancel").disabled(saving).on_click(cx.listener(|this, _, _, cx| {
                            this.title_editing = false;
                            cx.notify();
                        })))
                        .child(
                            Button::new("pr-title-save", Variant::Outline, Size::Xs)
                                .label(if saving { "Saving..." } else { "Save" })
                                .disabled(saving || empty)
                                .on_click(cx.listener(|this, _, _, cx| this.save_title(cx))),
                        ),
                )
                .into_any_element()
        } else {
            h_flex()
                .gap(px(6.))
                .min_w_0()
                .child(div().flex_1().min_w_0().truncate().text_base().line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title))
                .when(can_edit_title, |this| {
                    this.child(
                        Button::new("pr-title-edit", Variant::GhostMuted, Size::Xs)
                            .icon(Icon::Pencil)
                            .tooltip("Edit title")
                            .on_click(cx.listener(|this, _, window, cx| this.start_title_edit(window, cx))),
                    )
                })
                .into_any_element()
        };
        let entity = cx.entity().downgrade();
        let entity_retry = entity.clone();
        let stack_pill = interactions::stack_popover(
            self.entry.as_ref().and_then(|e| e.stack.as_ref()),
            self.stack.as_ref(),
            self.reference.number,
            move |number, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |view, cx| view.select_stack_layer(number, cx));
                }
            },
            move |cx| {
                if let Some(entity) = entity_retry.upgrade() {
                    entity.update(cx, |view, cx| view.retry_stack(cx));
                }
            },
            SharedString::from("pr-detail-stack-popover"),
            cx,
        );
        v_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(hsla(p.border))
            .child(
                h_flex()
                    .h(px(crate::workspace::TITLEBAR_HEIGHT))
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(format!("{} #{}", self.reference.repository, self.reference.number)),
                    )
                    .children(stack_pill)
                    .child(self.render_actions(window, cx))
                    .child(
                        Button::new("pr-detail-close", Variant::Ghost, Size::IconSm)
                            .icon(Icon::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_self(cx))),
                    ),
            )
            .child(title_row)
            .child(
                h_flex()
                    .gap(px(8.))
                    .min_w_0()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(h_flex().flex_none().gap(px(4.)).text_color(color).child(icon.sized(px(14.))).child(kind.label()))
                    .children(meta.map(|(text, additions, deletions, files)| {
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().child(text))
                            .child(diff_stat(additions, deletions, cx))
                            .child(div().flex_none().child(format!("{files} {}", if files == 1 { "file" } else { "files" })))
                    }))
                    .children(self.render_linked_threads(cx))
                    .children(url.map(|url| {
                        Button::new("pr-open-host", Variant::GhostMuted, Size::IconSm).icon(Icon::ExternalLink).tooltip("Open on the host").on_click(move |_, _, cx| cx.open_url(&url))
                    })),
            )
            .child(self.render_tabs(window, cx))
    }

    fn render_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        let option = |id: &'static str, icon: Icon, value: Tab, label: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            segment(id, icon, tab == value, label, window, cx).on_click(cx.listener(move |this, _, _, cx| this.set_tab(value, cx)))
        };
        h_flex().gap(px(8.)).child(
            segmented("pr-detail-tabs", cx)
                .child(option("pr-tab-summary", Icon::MessageSquare, Tab::Summary, "Summary", window, cx).child(tab_label("Summary")))
                .child(option("pr-tab-guide", Icon::FileText, Tab::Guide, "Guide", window, cx).child(tab_label("Guide")))
                .child(option("pr-tab-timeline", Icon::GitCommit, Tab::Timeline, "Timeline", window, cx).child(tab_label("Timeline")))
                .child(option("pr-tab-code", Icon::FileDiff, Tab::Code, "Code", window, cx).child(tab_label("Code"))),
        )
    }

    /// The primary control, the review trigger, the stack merge, the armed menu confirmation and the actions menu.
    /// An armed control that is no longer shown disarms, as the web's does on unmount.
    fn render_actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let Some(detail) = self.detail.clone() else { return h_flex().into_any_element() };
        let pending = mutations::pending(&self.reference, cx);
        let busy = pending.is_some();
        let gate = self.gate();
        let method = self.selected_merge_method(cx);
        let method_label = merge_method_label(method.unwrap_or(MergeMethod::Merge));
        let mut shown: Vec<&'static str> = Vec::new();
        let mut confirmable = |id: &'static str, button: Button, view: &Self, cx: &mut Context<Self>| {
            shown.push(id);
            if view.armed_on(id).is_some() { button.on_mouse_down_out(cx.listener(|this, _, _, cx| this.disarm(cx))) } else { button }
        };
        let blocked = gate.blocked.filter(|_| !gate.single && !gate.stack_owns);
        let review_button = (self.can_write_source_control(cx) && !allowed_review_verdicts(&detail).is_empty()).then(|| {
            let count = self.review_draft.comments.len();
            let label = if count > 0 { format!("Review ({count})") } else { "Review".to_owned() };
            Button::new("pr-open-review", if self.review_form_open || count > 0 { Variant::Outline } else { Variant::Ghost }, Size::Xs)
                .icon(Icon::MessageSquare)
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.review_form_open = !this.review_form_open;
                    cx.notify();
                }))
        });
        let primary: Option<AnyElement> = match primary_control(&detail, gate.stack_owns) {
            Some(PrimaryControl::Resolve) => Some(badge(format!("Conflicts with {}", detail.base_branch), BadgeKind::Outline, true, cx).text_color(hsla(p.destructive)).into_any_element()),
            Some(PrimaryControl::Ready) => Some(
                Button::new("pr-ready", Variant::Default, Size::Xs)
                    .icon(Icon::GitPullRequestArrow)
                    .label(if pending == Some(Action::Ready) { "Marking ready..." } else { "Ready for review" })
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.perform(Action::Ready, cx)))
                    .into_any_element(),
            ),
            Some(PrimaryControl::Merge) => {
                let armed = self.armed_on("pr-merge").is_some();
                let button = Button::new("pr-merge", Variant::Default, Size::Xs)
                    .icon(if armed { Icon::Check } else { Icon::GitMerge })
                    .label(if pending == Some(Action::Merge) { "Merging...".to_owned() } else if armed { "Confirm merge?".to_owned() } else { method_label.to_owned() })
                    .disabled(busy || blocked.is_some())
                    .when_some(blocked, |button, reason| button.tooltip(reason))
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::Merge), "pr-merge", cx)));
                Some(confirmable("pr-merge", button, self, cx).into_any_element())
            }
            Some(PrimaryControl::EnableAutoMerge) => {
                let armed = self.armed_on("pr-auto-merge").is_some();
                let label = format!("Auto-merge ({})", method_label.to_lowercase());
                let button = Button::new("pr-auto-merge", Variant::Default, Size::Xs)
                    .icon(if armed { Icon::Check } else { Icon::GitMerge })
                    .label(if pending == Some(Action::EnableAutoMerge) { "Enabling...".to_owned() } else if armed { "Confirm auto-merge?".to_owned() } else { label })
                    .disabled(busy || blocked.is_some())
                    .when_some(blocked, |button, reason| button.tooltip(reason))
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::EnableAutoMerge), "pr-auto-merge", cx)));
                Some(confirmable("pr-auto-merge", button, self, cx).into_any_element())
            }
            Some(PrimaryControl::AutoMergeArmed) => Some(badge("Auto-merge", BadgeKind::Info, true, cx).into_any_element()),
            Some(PrimaryControl::Merged | PrimaryControl::Closed) | None => None,
        };
        let stack = match &self.stack {
            Some(Ok(Some(stack))) if gate.stack_owns && detail.can(Action::Merge) && method.is_some() => {
                let layer_open = stack.layers.iter().any(|layer| layer.number == self.reference.number && layer.state == PrState::Open);
                layer_open.then(|| {
                    let merge = method.and_then(|method| stack_merge(stack, &self.reference, method));
                    let armed = self.armed_on("pr-merge-stack").is_some();
                    let term = |count: usize| if count == 1 { self.term().to_owned() } else { format!("{}s", self.term()) };
                    let tooltip = merge.as_ref().map_or_else(
                        || "A layer below is closed, a draft, or has no known head.".to_owned(),
                        |merge| format!("Merge stack through #{} into {} ({} {})", self.reference.number, stack.base, merge.count, term(merge.count)),
                    );
                    let label = if self.stack_pending {
                        "Merging stack...".to_owned()
                    } else if armed {
                        merge.as_ref().map_or_else(|| "Merge stack".to_owned(), |merge| format!("Confirm merging {} {}?", merge.count, term(merge.count)))
                    } else {
                        "Merge stack".to_owned()
                    };
                    let button = Button::new("pr-merge-stack", Variant::Default, Size::Xs)
                        .icon(if armed { Icon::Check } else { Icon::GitMerge })
                        .label(label)
                        .tooltip(tooltip)
                        .disabled(busy || merge.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Stack, "pr-merge-stack", cx)));
                    confirmable("pr-merge-stack", button, self, cx)
                })
            }
            _ => None,
        };
        let retry_stack = matches!(self.stack, Some(Err(_))).then(|| {
            Button::new("pr-retry-stack", Variant::Ghost, Size::Xs).label("Retry stack lookup").on_click(cx.listener(|this, _, _, cx| this.retry_stack(cx)))
        });
        let open = detail.state == PrState::Open;
        let can_merge_now = gate.single && open && !detail.is_draft && detail.can(Action::Merge) && method.is_some();
        let inline = self.armed_on(INLINE_CONFIRM).and_then(|confirm| match confirm {
            Confirm::Action(Action::Close) if open && detail.can(Action::Close) => {
                Some(Button::new(INLINE_CONFIRM, Variant::DestructiveOutline, Size::Xs).icon(Icon::Check).label(if pending == Some(Action::Close) { "Closing..." } else { "Confirm close?" }))
            }
            Confirm::Action(Action::Merge) if can_merge_now => Some(Button::new(INLINE_CONFIRM, Variant::Default, Size::Xs).icon(Icon::Check).label("Confirm merge?")),
            _ => None,
        });
        let inline = inline.map(|button| {
            let confirm = self.armed_on(INLINE_CONFIRM);
            let button = button.disabled(busy).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(confirm) = confirm {
                    this.confirm(confirm, INLINE_CONFIRM, cx);
                }
            }));
            confirmable(INLINE_CONFIRM, button, self, cx)
        });
        let workflows = detail.workflow_approvals_required.filter(|count| *count > 0 && open && detail.can(Action::ApproveWorkflows)).map(|_| {
            let armed = self.armed_on("pr-approve-workflows").is_some();
            let button = Button::new("pr-approve-workflows", Variant::WarningOutline, Size::Xs)
                .icon(if armed { Icon::Check } else { Icon::Hammer })
                .label(if pending == Some(Action::ApproveWorkflows) { "Approving..." } else if armed { "Confirm approve?" } else { "Approve workflows to run" })
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.confirm(Confirm::Action(Action::ApproveWorkflows), "pr-approve-workflows", cx)));
            confirmable("pr-approve-workflows", button, self, cx)
        });
        if self.armed.is_some_and(|armed| !shown.contains(&armed.element)) {
            self.armed = None;
            self._disarm = None;
            self._escape = None;
        }
        let menu = self.actions_menu(&detail, gate, cx);
        h_flex().flex_none().gap(px(4.)).children(retry_stack).children(workflows).children(review_button).children(stack).children(primary).children(inline).child(menu).into_any_element()
    }

    fn actions_menu(&self, detail: &Detail, gate: MergeGate, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let detail = detail.clone();
        let pending = mutations::pending(&self.reference, cx).is_some();
        let selected = self.selected_merge_method(cx);
        let refreshing = self.refreshing;
        let term = self.term();
        let active_link_state = self.is_linked_to_active_thread(cx);
        let thread_link_pending = self.thread_link_pending;
        let act = move |entity: &WeakEntity<DetailView>, run: ViewAction| {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |view, cx| run(view, cx));
                }
            }
        };
        Button::new("pr-actions", Variant::Ghost, Size::IconSm).icon(Icon::Ellipsis).tooltip("More actions").dropdown_menu(move |menu, _, _| {
            let mut menu = menu.item(PopupMenuItem::new(if refreshing { "Refreshing..." } else { "Refresh from host" }).disabled(refreshing).on_click(act(&entity, Box::new(|view, cx| view.refresh_from_host(cx)))));
            if detail.state == PrState::Open {
                menu = menu.separator();
                let toggle = if detail.is_draft { Action::Ready } else { Action::Draft };
                if detail.can(toggle) {
                    let label = if detail.is_draft { "Ready for review" } else { "Convert to draft" };
                    menu = menu.item(PopupMenuItem::new(label).disabled(pending).on_click(act(&entity, Box::new(move |view, cx| view.perform(toggle, cx)))));
                }
                if gate.single && !detail.is_draft && detail.can(Action::Merge) && selected.is_some() {
                    menu = menu.item(PopupMenuItem::new("Merge now").disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Confirm::Action(Action::Merge), INLINE_CONFIRM, cx)))));
                }
                if gate.single && detail.auto_merge_enabled == Some(true) && detail.can(Action::DisableAutoMerge) {
                    menu = menu.item(PopupMenuItem::new("Disable auto-merge").disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.perform(Action::DisableAutoMerge, cx)))));
                }
                let methods = detail.allowed_merge_methods();
                if methods.len() > 1 {
                    menu = menu.separator();
                    for method in methods {
                        menu = menu.item(
                            PopupMenuItem::new(merge_method_label(method))
                                .checked(selected == Some(method))
                                .disabled(pending)
                                .on_click(act(&entity, Box::new(move |view, cx| view.select_merge_method(method, cx)))),
                        );
                    }
                }
            }
            menu = menu.separator();
            if let Some((thread_id, linked_here)) = active_link_state.clone() {
                let label = if linked_here { "Unlink from this thread" } else { "Link to this thread" };
                menu = menu.item(PopupMenuItem::new(label).disabled(thread_link_pending).on_click(act(
                    &entity,
                    Box::new(move |view, cx| view.link_to_thread(thread_id.clone(), linked_here, cx)),
                )));
            }
            menu = menu.item(PopupMenuItem::new("Link to thread...").disabled(thread_link_pending).on_click(act(&entity, Box::new(|view, cx| view.open_thread_picker(cx)))));
            let url = detail.url.clone();
            let link = detail.url.clone();
            let number = format!("#{}", detail.number);
            menu = menu
                .separator()
                .item(PopupMenuItem::new("Open on the host").on_click(move |_, _, cx| cx.open_url(&url)))
                .item(PopupMenuItem::new(format!("Copy {term} link")).on_click(act(&entity, Box::new(move |view, cx| view.copy(link.clone(), "Link", cx)))))
                .item(PopupMenuItem::new(format!("Copy {term} number")).on_click(act(&entity, Box::new(move |view, cx| view.copy(number.clone(), "Number", cx)))));
            if detail.state == PrState::Open && detail.can(Action::Close) {
                menu = menu
                    .separator()
                    .item(PopupMenuItem::new(format!("Close {term}")).disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.confirm(Confirm::Action(Action::Close), INLINE_CONFIRM, cx)))));
            } else if detail.state == PrState::Closed && detail.can(Action::Reopen) {
                menu = menu.separator().item(PopupMenuItem::new(format!("Reopen {term}")).disabled(pending).on_click(act(&entity, Box::new(|view, cx| view.perform(Action::Reopen, cx)))));
            }
            menu
        })
    }

    fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        if let Some(page) = self.page.upgrade() {
            let what = what.to_owned();
            page.update(cx, |page, cx| page.copy(text, &what, cx));
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            let toast = Toast::new(ToastKind::Success, format!("{what} copied")).description(text);
            self.state.update(cx, |state, cx| state.show_toast(toast, cx));
        }
    }

    fn render_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            Tab::Summary => super::summary::render(self, window, cx),
            Tab::Guide => self.guide.clone().map_or_else(|| div().into_any_element(), |guide| div().flex_1().min_h_0().flex().flex_col().child(guide).into_any_element()),
            Tab::Timeline => super::timeline::render(self, window, cx),
            Tab::Code => self.code.clone().map_or_else(|| div().into_any_element(), |code| div().flex_1().min_h_0().flex().flex_col().child(code).into_any_element()),
        }
    }
}

fn tab_label(label: &'static str) -> impl IntoElement {
    div().ml(px(6.)).text_xs().font_weight(FontWeight::MEDIUM).child(SharedString::from(label))
}

impl Render for DetailView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        self.markdown.begin_frame();
        let review_panel = self.detail.clone().and_then(|detail| interactions::review_form_panel(self, &detail, window, cx));
        let thread_picker = interactions::thread_link_picker_modal(self, window, cx);
        let body = match (&self.detail, &self.error) {
            (None, Some(error)) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(24.))
                .text_center()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child("Could not load this pull request"))
                .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(error.clone()))
                .child(Button::new("pr-detail-retry", Variant::Outline, Size::Sm).icon(Icon::RefreshCw).label("Retry").on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                .into_any_element(),
            (None, None) => super::row::ghost("", cx).into_any_element(),
            (Some(_), _) => self.render_tab(window, cx),
        };
        if self._activation.is_none() {
            self._activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.disarm(cx);
                }
            }));
        }
        v_flex()
            .id("pr-detail")
            .relative()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(self.render_header(window, cx))
            .children(review_panel)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .children(thread_picker)
            .child(self.viewer.clone())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use t3_core::pull_requests::wire::{Activity, Detail, Summary, decode};

    use super::*;

    fn sample_ref() -> PrRef {
        PrRef { project_id: "p1".into(), host: Some("github.com".into()), repository: "upstream/repo".into(), number: 42 }
    }

    fn sample_detail() -> Detail {
        decode(json!({
            "provider": "github",
            "capabilities": { "actions": ["merge", "close"], "mergeMethods": ["merge"], "edit": { "changeRequest": true, "comment": true }, "review": { "resolve": true }, "labels": true },
            "viewerPermissions": { "actions": ["merge", "close"], "comment": true, "resolve": true, "labels": true },
            "projectId": "p1", "projectTitle": "Local Fork", "workspaceRoot": "/tmp/fork-checkout", "repository": "upstream/repo", "number": 42,
            "title": "PR Title", "body": "- [ ] Ship it", "url": "https://github.com/upstream/repo/pull/42", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": true, "mergeability": "mergeable", "additions": 5, "deletions": 2, "changedFiles": 1,
            "headBranch": "feat", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap()
    }

    fn sample_activity() -> Activity {
        decode(json!({
            "comments": [{
                "id": "c1", "kind": "issue-comment", "author": { "login": "bob" },
                "body": "Please rename this helper", "createdAt": "2026-01-02T01:00:00Z",
                "url": "https://github.com/upstream/repo/pull/42#issuecomment-1"
            }],
            "commentCount": 1, "commentsTruncated": false, "reviewThreads": [], "overviewThreads": [], "commits": []
        }))
        .unwrap()
    }

    #[gpui::test]
    fn held_cache_paints_immediately_and_media_scope_preserves_pr_repository(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let detail = sample_detail();
        let activity = sample_activity();
        let reference = sample_ref();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| state.environments.active = Some("env-a".into()));
            let key = HeldKey::of(state.read(cx), &reference).expect("active env keys Held");
            hold(key, Some(&detail), Some(&activity), cx);
            let view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), reference.clone(), None, cx));
            view.update(cx, |view, cx| {
                // Reopening in the same environment & project paints immediately from held detail & activity.
                assert_eq!(view.detail.as_ref().map(|d| d.title.as_str()), Some("PR Title"));
                assert_eq!(view.activity.as_ref().map(|a| a.comments.len()), Some(1));
                assert!(!view.timeline.is_empty());
                // Media scope preserves the PR's own repository (`upstream/repo`), distinct from the local checkout.
                let scope = view.media_scope(&detail, cx);
                assert!(scope.source_control_media);
                assert_eq!(scope.source_control_media_repository.as_deref(), Some("upstream/repo"));
                assert_eq!(scope.workspace_root.as_deref(), Some("/tmp/fork-checkout"));
            });

            // Cross-project isolation: another project/worktree pointing at the same repo + PR # never paints `p1`'s cache.
            let other_project_ref = PrRef { project_id: "p2".into(), ..reference.clone() };
            let other_project_view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), other_project_ref, None, cx));
            assert!(other_project_view.read(cx).detail.is_none());
            assert!(other_project_view.read(cx).activity.is_none());

            // Cross-environment isolation: switching environments never paints `env-a`'s cached detail.
            state.update(cx, |state, _| state.environments.active = Some("env-b".into()));
            let other_env_view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), reference.clone(), None, cx));
            assert!(other_env_view.read(cx).detail.is_none());
            assert!(other_env_view.read(cx).activity.is_none());

            // Disconnected (`environments.active = None`) neither reads nor writes `Held`.
            state.update(cx, |state, _| state.environments.active = None);
            assert!(HeldKey::of(state.read(cx), &reference).is_none());
            let disconnected_view = cx.new(|cx| DetailView::new(state, WeakEntity::new_invalid(), reference, None, cx));
            assert!(disconnected_view.read(cx).detail.is_none());
        });
    }

    #[gpui::test]
    fn action_reply_reveal_anchors_and_add_comment_to_chat_work_end_to_end(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let detail = sample_detail();
        let activity = sample_activity();
        let reference = sample_ref();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                state.environments.active = Some("env-a".into());
                state.selected_thread = Some("thread-1".into());
            });
            let key = HeldKey::of(state.read(cx), &reference).unwrap();
            hold(key, Some(&detail), Some(&activity), cx);
            let view = cx.new(|cx| DetailView::new(state.clone(), WeakEntity::new_invalid(), reference.clone(), None, cx));

            // Action reply patches the shown detail immediately without requiring a re-read.
            let reply: Summary = serde_json::from_value(json!({
                "state": "open", "isDraft": false, "updatedAt": "2026-01-03T00:00:00Z"
            }))
            .unwrap();
            let reread = view.update(cx, |view, cx| view.apply_reply(Some(&reply), cx));
            assert!(!reread);
            assert_eq!(view.read(cx).detail.as_ref().map(|d| d.is_draft), Some(false));

            // Link navigation routes checks -> Summary (checks open), comment -> Timeline, diff -> Code.
            view.update(cx, |view, cx| view.reveal("https://github.com/upstream/repo/pull/42/checks", cx));
            assert_eq!(view.read(cx).tab, Tab::Summary);
            assert!(view.read(cx).checks_open);

            view.update(cx, |view, cx| view.reveal("https://github.com/upstream/repo/pull/42#issuecomment-1", cx));
            assert_eq!(view.read(cx).tab, Tab::Timeline);

            view.update(cx, |view, cx| view.reveal("https://github.com/upstream/repo/pull/42/files", cx));
            assert_eq!(view.read(cx).tab, Tab::Code);
            assert!(view.read(cx).code.is_some());

            // Adding a PR comment to chat queues the PR context chip first and then the comment chip.
            let pr_chip = view.update(cx, |view, cx| {
                view.add_comment_to_chat(None, "c1", cx);
                state.update(cx, |state, _| state.take_review_comment("thread-1"))
            });
            assert_eq!(pr_chip.as_ref().map(|chip| chip.id.as_str()), Some("pull-request-context:https://github.com/upstream/repo/pull/42"));
            state
        });
        // Deferred comment chip flushes at the end of the previous `cx.update`.
        cx.update(|cx| {
            let comment_chip = state.update(cx, |state, _| state.take_review_comment("thread-1"));
            assert_eq!(comment_chip.as_ref().map(|chip| chip.id.as_str()), Some("pr-comment:https://github.com/upstream/repo/pull/42:comment:c1"));
            assert_eq!(comment_chip.as_ref().map(|chip| &*chip.text), Some("bob: Please rename this helper"));
        });
    }

    #[gpui::test]
    fn label_candidates_failure_clears_inflight_task_so_reopening_can_retry(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let reference = sample_ref();
        let view = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            let view = cx.new(|cx| DetailView::new(state, WeakEntity::new_invalid(), reference, None, cx));
            view.update(cx, |view, cx| view.load_label_candidates(cx));
            view
        });
        // Disconnected `read()` settles synchronously with `Err(NOT_CONNECTED)` on the foreground executor.
        cx.run_until_parked();
        cx.update(|cx| {
            let view = view.read(cx);
            assert!(matches!(view.label_candidates, Some(Err(_))));
            assert!(view._labels.is_none(), "error must clear `_labels` so retry is not locked out");
        });
    }

    #[gpui::test]
    fn guide_tab_tour_code_tab_hooks_and_inline_review_draft_work_together(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let detail: Detail = decode(json!({
            "provider": "github",
            "capabilities": {
                "actions": ["merge", "close"],
                "mergeMethods": ["merge"],
                "edit": { "changeRequest": true, "comment": true },
                "review": { "resolve": true, "inlineComment": true, "rangeComment": true, "verdicts": ["approve", "comment", "request-changes"] },
                "reviewers": { "request": true, "remove": true, "searchCandidates": true },
                "labels": true,
                "viewedFiles": "host"
            },
            "viewerPermissions": { "actions": ["merge", "close"], "comment": true, "resolve": true, "labels": true, "requestReviewers": true },
            "projectId": "p1", "projectTitle": "Local Fork", "workspaceRoot": "/tmp/fork-checkout", "repository": "upstream/repo", "number": 42,
            "title": "PR Title", "body": "- [ ] Ship it", "url": "https://github.com/upstream/repo/pull/42", "author": { "login": "alice" },
            "viewer": "alice", "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 5, "deletions": 2, "changedFiles": 1,
            "headBranch": "feat", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "observedAt": 100, "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": { "merge": true, "squash": false, "rebase": false }
        }))
        .unwrap();
        let activity: Activity = decode(json!({
            "comments": [],
            "commentCount": 0,
            "commentsTruncated": false,
            "reviewThreads": [{
                "id": "rt1",
                "path": "src/lib.rs",
                "line": 10,
                "side": "right",
                "isResolved": false,
                "isOutdated": false,
                "comments": [{
                    "id": "rc1",
                    "author": { "login": "bob" },
                    "body": "Check bounds here",
                    "createdAt": "2026-01-02T01:00:00Z"
                }]
            }],
            "overviewThreads": [],
            "commits": []
        }))
        .unwrap();
        let guide_result: t3_core::pull_requests::wire::ReviewGuideResult = decode(json!({
            "guide": {
                "headSha": "abc1234",
                "generatedAt": "2026-01-02T02:00:00Z",
                "summary": "Overview of changes",
                "stops": [
                    {
                        "title": "First stop",
                        "summary": "Core logic",
                        "attention": { "kind": "close-read", "note": "Check edge cases" },
                        "files": [{ "path": "src/lib.rs", "note": "Main entry" }],
                        "excerpts": [],
                        "notes": [],
                        "questions": ["Is bounds check needed?"]
                    },
                    {
                        "title": "Second stop",
                        "summary": "Tests",
                        "attention": null,
                        "files": [{ "path": "src/lib.rs" }],
                        "excerpts": [],
                        "notes": [],
                        "questions": []
                    }
                ]
            },
            "run": null,
            "currentHeadSha": "abc1234"
        }))
        .unwrap();
        let reference = sample_ref();
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| state.environments.active = Some("env-a".into()));
            let key = HeldKey::of(state.read(cx), &reference).unwrap();
            hold(key, Some(&detail), Some(&activity), cx);
            let view = cx.new(|cx| DetailView::new(state, WeakEntity::new_invalid(), reference, None, cx));

            // Guide tab tour stepping + jump to Code tab file.
            view.update(cx, |view, cx| view.set_tab(Tab::Guide, cx));
            let guide = view.read(cx).guide.clone().expect("GuideTab created");
            guide.update(cx, |guide, cx| guide.set_result_for_test(guide_result, Vec::new(), cx));
            assert_eq!(guide.read(cx).current_position(), 0);
            guide.update(cx, |guide, cx| guide.step(1, cx));
            assert_eq!(guide.read(cx).current_position(), 1);
            guide.update(cx, |guide, cx| guide.step(-1, cx));
            assert_eq!(guide.read(cx).current_position(), 0);

            // Jumping to a file opens the Code tab and mounts DiffPanel with viewed-files & review-thread hooks.
            view.update(cx, |view, cx| view.open_code_file("src/lib.rs".into(), cx));
            assert_eq!(view.read(cx).tab, Tab::Code);
            let code = view.read(cx).code.clone().expect("CodeTab created");
            let _ = code.read(cx).diff_panel();

            // Inline comment from DiffPanel lands in DetailView's pending review draft and opens the review panel.
            view.update(cx, |view, cx| {
                view.add_pending_review_comment(
                    PendingReviewComment {
                        id: "pending-1".into(),
                        path: "src/lib.rs".into(),
                        old_path: None,
                        position: t3_core::pull_requests::review::ReviewPosition::Added { new_line: 10 },
                        start_position: None,
                        range_label: "L10".into(),
                        body: "Please handle empty input".into(),
                    },
                    cx,
                );
            });
            assert_eq!(view.read(cx).pending_review_comment_count(), 1);
            view.update(cx, |view, cx| view.open_review_form(cx));
            assert!(view.read(cx).review_form_open);
        });
    }

    #[gpui::test]
    fn hidden_tag_filtering_hides_tagged_linked_threads_and_embedded_hooks_fire(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let reference = sample_ref();
        let closed = Rc::new(std::cell::Cell::new(false));
        let selected_layer = Rc::new(std::cell::Cell::new(0u64));
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            super::super::mutations::init(runtime.handle().clone(), cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            state.update(cx, |state, _| {
                state.shell.snapshot = Some(serde_json::from_value(json!({
                    "snapshotSequence": 1,
                    "projects": [{
                        "id": "p1",
                        "title": "Repo",
                        "workspaceRoot": "/tmp/repo",
                        "defaultModelSelection": null,
                        "scripts": [],
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }],
                    "threads": [
                        {
                            "id": "t-visible", "projectId": "p1", "title": "Visible thread", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t-visible" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "tags": [], "pullRequests": []
                        },
                        {
                            "id": "t-hidden", "projectId": "p1", "title": "Hidden thread", "providerInstanceId": "replay",
                            "modelSelection": { "instanceId": "replay", "model": "replay" },
                            "runtimeMode": "full-access", "interactionMode": "default",
                            "lineage": { "rootThreadId": "t-hidden" }, "status": "idle",
                            "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
                            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                            "tags": ["archived"], "pullRequests": []
                        }
                    ],
                    "updatedAt": "2026-01-02T00:00:00Z"
                })).unwrap());
                state.sidebar.hidden_thread_ids.insert("t-hidden".into());
            });
            let closed_cb = closed.clone();
            let selected_cb = selected_layer.clone();
            let view = cx.new(|cx| {
                DetailView::new_embedded(
                    state,
                    reference,
                    None,
                    Rc::new(move |_| closed_cb.set(true)),
                    Rc::new(move |pr, _| selected_cb.set(pr.number)),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.linked_threads = Some(vec![
                    LinkedThread {
                        id: "t-visible".into(),
                        project_id: "p1".into(),
                        title: "Visible thread".into(),
                        archived_at: None,
                    },
                    LinkedThread {
                        id: "t-hidden".into(),
                        project_id: "p1".into(),
                        title: "Hidden thread".into(),
                        archived_at: None,
                    },
                ]);
                let visible = view.visible_linked_threads(cx);
                assert_eq!(visible.len(), 1);
                assert_eq!(visible[0].id, "t-visible");
                view.select_stack_layer(99, cx);
                view.close_self(cx);
            });
            assert_eq!(selected_layer.get(), 99);
            assert!(closed.get());
        });
    }
}
