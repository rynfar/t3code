//! The pull request detail panel (`PullRequestDetailPanel.tsx`): its reads, the header with the
//! primary control and the actions menu (`PullRequestDetailActions.tsx`,
//! `PullRequestActionsMenu.tsx`) with the web's two-step confirmations
//! (`useInlineActionConfirm.ts`), the stack merge where the host runs stack actions
//! (`PullRequestStackMenu.tsx`), and the Summary, Timeline and Code tabs. It paints at once
//! from what was last read for the pull request (`usePullRequestDetail`'s held answers), applies
//! an action's reply without a re-read, writes the description's task checkboxes back, resolves
//! conversation threads, edits labels, and goes where a link's fragment points.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, Global, IntoElement, ParentElement, Render, ScrollHandle, SharedString, Styled, Subscription, Task, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use t3_core::pull_requests::detail::{MergeGate, PrimaryControl, StateKind, change_request_term, host_label, merge_gate, merge_method_label, primary_control, resolve_merge_method};
use t3_core::pull_requests::anchor::{Anchor, LinkedTab, anchor_matches_url, link_commit, parse_anchor, resolve_review_anchor};
use t3_core::pull_requests::chat_context::{ChatSubject, build_comment_context, build_pull_request_context};
use t3_core::pull_requests::detail::EventKind;
use t3_core::pull_requests::link::{LINKED_THREADS, LINKED_THREADS_POLL_SECONDS, LinkedThread, LinkedThreads, SUBSCRIBE_LINKED_THREAD_REFRESHES, linked_threads_input, linked_threads_label};
use t3_core::pull_requests::editing::{
    self, DescriptionDraft, SOURCE_CONTROL_WRITE_SCOPE, apply_label_change, can_edit_change_request, description_draft_in_force, label_change_input, patch_detail, settle_description_save,
    start_description_save, thread_resolution_input, update_body_input,
};
use t3_core::pull_requests::tasks::set_task_checked;
use t3_core::pull_requests::wire::{Action, Activity, Detail, LabelCandidates, ListEntry, MergeMethod, PrState, Stack, Summary, decode};
use t3_core::pull_requests::{PrRef, action_failed_label, action_input, action_needs_host_refresh, invalidate_input, method, stack_merge};

use super::code::CodeTab;
use super::controls::{BadgeKind, Button, Size, Variant, badge, diff_stat, state_glyph};
use super::mutations::{self, MergePicks, Mutations};
use super::page::{PullRequestsPage, Refreshes};
use super::{read, settle};
use crate::app_state::{AppEvent, AppState};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Summary,
    Timeline,
    Code,
}

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
    /// `pullRequests.stack` where the host runs stack actions; `None` until it first answers.
    stack: Option<Result<Option<Stack>, String>>,
    stack_pending: bool,
    refreshing: bool,
    armed: Option<Armed>,
    /// A description written from here, shown until a host reading after its saves.
    description_draft: Option<DescriptionDraft>,
    description_saves: u32,
    /// `pullRequests.labelCandidates`, read once the label picker can show.
    pub(super) label_candidates: Option<Result<LabelCandidates, String>>,
    pub(super) label_pending: bool,
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
            description_draft: None,
            description_saves: 0,
            label_candidates: None,
            label_pending: false,
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

    /// The header's "Linked from N threads" (`PullRequestThreadLinks`'s count): each thread
    /// opens from its menu.
    fn render_linked_threads(&self) -> Option<AnyElement> {
        let threads = self.linked_threads.clone().filter(|threads| !threads.is_empty())?;
        let label = linked_threads_label(threads.len());
        let state = self.state.clone();
        Some(
            Button::new("pr-linked-threads", Variant::GhostMuted, Size::Xs)
                .icon(Icon::MessageSquare)
                .label(threads.len().to_string())
                .tooltip(label)
                .dropdown_menu(move |mut menu, _, _| {
                    for thread in &threads {
                        let (id, state) = (thread.id.clone(), state.clone());
                        let title = if thread.title.trim().is_empty() { "Untitled thread".to_owned() } else { thread.title.clone() };
                        menu = menu.item(PopupMenuItem::new(title).on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| {
                                state.set_pull_requests_open(false, cx);
                                state.select_thread(&id, cx);
                            });
                        }));
                    }
                    menu
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

    /// Stack membership, read only where the host runs stack actions. The last answer stands
    /// while it is re-read; the merge itself checks again before it runs.
    fn refresh_stack(&mut self, cx: &mut Context<Self>) {
        let supported = self.detail.as_ref().is_some_and(|detail| detail.capabilities.stacks == Some(true) && detail.capabilities.stack_actions == Some(true));
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
            let reference = self.reference.clone();
            self.code = Some(cx.new(|cx| CodeTab::new(state, reference, cx)));
        }
        cx.notify();
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
                    .child(self.render_actions(window, cx))
                    .child(
                        Button::new("pr-detail-close", Variant::Ghost, Size::IconSm)
                            .icon(Icon::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(page) = this.page.upgrade() {
                                    page.update(cx, |page, cx| page.close_detail(cx));
                                }
                            })),
                    ),
            )
            .child(div().text_base().line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.foreground)).child(title))
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
                    .children(self.render_linked_threads())
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
                .child(option("pr-tab-timeline", Icon::GitCommit, Tab::Timeline, "Timeline", window, cx).child(tab_label("Timeline")))
                .child(option("pr-tab-code", Icon::FileDiff, Tab::Code, "Code", window, cx).child(tab_label("Code"))),
        )
    }

    /// The primary control, the stack merge, the armed menu confirmation and the actions menu.
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
        h_flex().flex_none().gap(px(4.)).children(retry_stack).children(workflows).children(stack).children(primary).children(inline).child(menu).into_any_element()
    }

    fn actions_menu(&self, detail: &Detail, gate: MergeGate, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let detail = detail.clone();
        let pending = mutations::pending(&self.reference, cx).is_some();
        let selected = self.selected_merge_method(cx);
        let refreshing = self.refreshing;
        let term = self.term();
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
        }
    }

    fn render_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            Tab::Summary => super::summary::render(self, window, cx),
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
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
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
}
