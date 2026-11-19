//! Issue detail panel (`apps/web/src/components/issue/IssueDetailPanel.tsx`,
//! `IssueCommentComposer.tsx`, `IssueDetailGhost.tsx`, `IssueThreadLinks.tsx`,
//! `pingdotgg/t3code#19260`, `#19270`, `#37930`, `#38489`, `#38537`, `#38771`, `#39000`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, KeyDownEvent, ParentElement, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};
use serde_json::json;
use t3_core::issues::pager::IssueCommentPager;
use t3_core::issues::presentation::{
    IssueTimelineItem, issue_event_sentence, issue_write_access, merge_issue_timeline,
};
use t3_core::issues::selection::{
    IssuePicks, SelectionEntry, issue_assignee_held, issue_assignees_after_picks,
    issue_labels_after_picks, issue_picks_after_failed_write, issue_picks_for,
    issue_write_after_picks,
};
use t3_core::issues::wire::{
    IssueAssigneeCandidate, IssueCandidates, IssueCommentInput, IssueCommentsPage,
    IssueCommentsPageInput, IssueDetail, IssueLabelCandidate, IssueLinkedPullRequest,
    IssueListEntry, IssueReaction, IssueReactionContent, IssueReactionInput, IssueRef,
    IssueScopeRef, IssueSetStateInput, IssueUpdateInput, ThreadIssueKey, decode, method,
};
use t3_core::pull_requests::PrRef;
use t3_core::pull_requests::detail::StateKind as PrStateKind;
use t3_core::pull_requests::list::label_color;
use t3_core::pull_requests::reactions::{REACTION_ORDER, reaction_emoji, reaction_name};
use t3_core::sidebar::time::{compact_relative_label, parse_iso};

use super::ghost::{render_ghost, render_unavailable};
use super::thread_links::{self, IssueThreadLinksState};
use super::{
    HeldIssueKey, can_write_issues, environment_owner_for, held_issue_detail, held_issue_seed,
    hold_issue_detail, mark_issues_unsupported, read_for_owner, settle, state_glyph,
    write_for_owner,
};
use crate::app_state::{AppEvent, AppState};
use crate::i18n::t;
use crate::icons::Icon;
use crate::pull_requests::controls::{
    BadgeKind, Button, Size, Variant, avatar, badge, state_glyph as pr_state_glyph,
};
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::markdown::links::MarkdownActionContext;
use crate::transcript::markdown::{MarkdownStore, prose};

pub(crate) type CloseHandler = Rc<dyn Fn(&mut App)>;
pub(crate) type SelectPrHandler = Rc<dyn Fn(PrRef, &mut App)>;
pub(crate) type MutatedHandler = Rc<dyn Fn(&mut App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Title,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidatePickerKind {
    Labels,
    Assignees,
}

pub struct IssueDetailPanel {
    state: Entity<AppState>,
    environment_id: Option<String>,
    reference: IssueRef,
    /// Present when opened beside a thread in the right panel; `None` on the standalone Issues page.
    thread_id: Option<String>,
    seed: Option<IssueListEntry>,
    detail: Option<IssueDetail>,
    detail_revision: u64,
    pending: bool,
    /// Failure on initial load or latest refresh (`pingdotgg/t3code#38537`).
    error: Option<String>,
    pager: IssueCommentPager,
    thread_links: IssueThreadLinksState,
    editing: Option<EditField>,
    title_input: Option<Entity<InputState>>,
    body_input: Option<Entity<TextareaState>>,
    saving: bool,
    changing_state: bool,
    comment_input: Option<Entity<TextareaState>>,
    comment_draft: String,
    comment_submitting: bool,
    _comment_sub: Option<Subscription>,
    _title_sub: Option<Subscription>,
    picker: Option<CandidatePickerKind>,
    picker_input: Option<Entity<InputState>>,
    picker_query: String,
    _picker_sub: Option<Subscription>,
    candidates: Option<IssueCandidates>,
    candidates_pending: bool,
    candidates_error: Option<String>,
    label_picks: IssuePicks<IssueLabelCandidate>,
    assignee_picks: IssuePicks<IssueAssigneeCandidate>,
    markdown: MarkdownStore,
    on_close: Option<CloseHandler>,
    on_select_pr: Option<SelectPrHandler>,
    on_mutated: Option<MutatedHandler>,
    _fetch: Option<Task<()>>,
    _older: Option<Task<()>>,
    _candidates: Option<Task<()>>,
    next_mutation_id: u64,
    _mutations: HashMap<u64, Task<()>>,
    _subscription: Subscription,
}

impl IssueDetailPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: Entity<AppState>,
        environment_id: Option<String>,
        reference: IssueRef,
        thread_id: Option<String>,
        seed: Option<IssueListEntry>,
        on_close: Option<CloseHandler>,
        on_select_pr: Option<SelectPrHandler>,
        on_mutated: Option<MutatedHandler>,
        cx: &mut Context<Self>,
    ) -> Self {
        let held_key = HeldIssueKey::of(state.read(cx), environment_id.as_deref(), &reference);
        let initial_detail = held_key
            .as_ref()
            .and_then(|key| held_issue_detail(key, cx));
        let initial_seed = seed.or_else(|| {
            held_key
                .as_ref()
                .and_then(|key| held_issue_seed(key, cx))
        });
        let subscription = cx.subscribe(&state, |_, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Shell | AppEvent::Phase | AppEvent::ServerConfig) {
                cx.notify();
            }
        });
        let mut panel = Self {
            state,
            environment_id,
            reference,
            thread_id,
            seed: initial_seed,
            detail: initial_detail,
            detail_revision: 1,
            pending: false,
            error: None,
            pager: IssueCommentPager::default(),
            thread_links: IssueThreadLinksState::default(),
            editing: None,
            title_input: None,
            body_input: None,
            saving: false,
            changing_state: false,
            comment_input: None,
            comment_draft: String::new(),
            comment_submitting: false,
            _comment_sub: None,
            _title_sub: None,
            picker: None,
            picker_input: None,
            picker_query: String::new(),
            _picker_sub: None,
            candidates: None,
            candidates_pending: false,
            candidates_error: None,
            label_picks: IssuePicks::empty(),
            assignee_picks: IssuePicks::empty(),
            markdown: MarkdownStore::new(cx.entity_id()),
            on_close,
            on_select_pr,
            on_mutated,
            _fetch: None,
            _older: None,
            _candidates: None,
            next_mutation_id: 0,
            _mutations: HashMap::new(),
            _subscription: subscription,
        };
        panel.fetch_detail(false, cx);
        if let Some(detail) = panel.detail.clone() {
            panel.sync_linked_threads(&detail, cx);
        }
        panel
    }

    pub fn matches_target(&self, environment_id: Option<&str>, reference: &IssueRef) -> bool {
        self.environment_id.as_deref() == environment_id && &self.reference == reference
    }

    pub fn retarget(
        &mut self,
        environment_id: Option<String>,
        reference: IssueRef,
        seed: Option<IssueListEntry>,
        cx: &mut Context<Self>,
    ) {
        if self.environment_id == environment_id && self.reference == reference {
            if self.seed.is_none() && seed.is_some() {
                self.seed = seed;
                cx.notify();
            }
            return;
        }
        let held_key = HeldIssueKey::of(self.state.read(cx), environment_id.as_deref(), &reference);
        self.environment_id = environment_id;
        self.reference = reference;
        self.seed = seed.or_else(|| {
            held_key
                .as_ref()
                .and_then(|key| held_issue_seed(key, cx))
        });
        self.detail = held_key
            .as_ref()
            .and_then(|key| held_issue_detail(key, cx));
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.pending = false;
        self.error = None;
        self.pager.reset();
        self.thread_links.reset();
        self.editing = None;
        self.title_input = None;
        self.body_input = None;
        self._title_sub = None;
        self.saving = false;
        self.changing_state = false;
        self.picker = None;
        self.picker_input = None;
        self.picker_query.clear();
        self._picker_sub = None;
        self.candidates = None;
        self.candidates_pending = false;
        self.candidates_error = None;
        self.label_picks = IssuePicks::empty();
        self.assignee_picks = IssuePicks::empty();
        self.comment_draft.clear();
        self.comment_input = None;
        self.comment_submitting = false;
        self._comment_sub = None;
        self._fetch = None;
        self._older = None;
        self._candidates = None;
        self._mutations.clear();
        self.fetch_detail(false, cx);
        if let Some(detail) = self.detail.clone() {
            self.sync_linked_threads(&detail, cx);
        }
        cx.notify();
    }

    fn issue_key(&self, detail: &IssueDetail) -> ThreadIssueKey {
        let host = self
            .reference
            .host
            .clone()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| detail.host.clone());
        ThreadIssueKey {
            tracker: self.reference.tracker.clone(),
            host,
            scope: self.reference.scope.clone(),
            key: self.reference.key.clone(),
        }
    }

    fn sync_linked_threads(&mut self, detail: &IssueDetail, cx: &mut Context<Self>) {
        let key = self.issue_key(detail);
        let env = self.environment_id.clone();
        self.thread_links.fetch(
            &self.state,
            env.as_deref(),
            &key,
            |this| &mut this.thread_links,
            cx,
        );
        self.thread_links.ensure_subscription(
            &self.state,
            env,
            key,
            |this| &mut this.thread_links,
            cx,
        );
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.fetch_detail(true, cx);
    }

    fn fetch_detail(&mut self, is_refresh: bool, cx: &mut Context<Self>) {
        if is_refresh && !self.pager.begin_refresh() {
            return;
        }
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = read_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::DETAIL,
            self.reference.payload(),
            decode::<IssueDetail>,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        self.pending = true;
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                panel.apply_detail_result(is_refresh, result, cx);
            });
        }));
        cx.notify();
    }

    fn apply_detail_result(
        &mut self,
        is_refresh: bool,
        result: Result<IssueDetail, super::ReadFailure>,
        cx: &mut Context<Self>,
    ) {
        self.pending = false;
        match result {
            Ok(detail) => {
                if let Some(key) = HeldIssueKey::of(
                    self.state.read(cx),
                    self.environment_id.as_deref(),
                    &self.reference,
                ) {
                    hold_issue_detail(key, &detail, cx);
                }
                let first_load = self.detail.is_none();
                self.detail = Some(detail.clone());
                self.detail_revision = self.detail_revision.wrapping_add(1);
                self.error = None;
                if is_refresh {
                    self.pager.finish_refresh(true);
                }
                if first_load {
                    self.sync_linked_threads(&detail, cx);
                }
            }
            Err(failure) => {
                if failure.unsupported
                    && let Some(env_id) = self
                        .environment_id
                        .as_deref()
                        .or_else(|| self.state.read(cx).environment())
                        .or(self.state.read(cx).environments.active.as_deref())
                        .map(str::to_owned)
                {
                    mark_issues_unsupported(&env_id, cx);
                }
                self.error = Some(failure.message);
                if is_refresh {
                    // Preserves previously loaded older comments on failed refresh (`#39000`).
                    self.pager.finish_refresh(false);
                }
            }
        }
        cx.notify();
    }

    fn load_older_comments(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.pager.begin_load_older(self.detail.as_ref(), self.pending) else {
            return;
        };
        let input = IssueCommentsPageInput {
            reference: self.reference.clone(),
            cursor: request.cursor.clone(),
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = read_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::COMMENTS,
            payload,
            decode::<IssueCommentsPage>,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        self._older = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                match result {
                    Ok(page) => {
                        panel.pager.finish_load_older(&request, Ok(page));
                    }
                    Err(failure) => {
                        panel.pager.finish_load_older(&request, Err(()));
                        let title = t(cx, "Older comments could not be read").to_owned();
                        panel.state.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn alloc_mutation_id(&mut self) -> u64 {
        let id = self.next_mutation_id;
        self.next_mutation_id = self.next_mutation_id.wrapping_add(1);
        id
    }

    fn move_to_state(&mut self, next_state: String, cx: &mut Context<Self>) {
        if self.changing_state {
            return;
        }
        let input = IssueSetStateInput {
            reference: self.reference.clone(),
            state: next_state,
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = write_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::SET_STATE,
            payload,
            Ok,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        let mutation_id = self.alloc_mutation_id();
        self.changing_state = true;
        let handle = cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                panel._mutations.remove(&mutation_id);
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                panel.changing_state = false;
                match result {
                    Ok(_) => {
                        panel.notify_mutated(cx);
                        panel.fetch_detail(true, cx);
                    }
                    Err(failure) => {
                        let title = t(cx, "The state could not be changed").to_owned();
                        panel.state.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        });
        self._mutations.insert(mutation_id, handle);
        cx.notify();
    }

    fn start_edit_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.detail.as_ref().map(|d| d.title.clone()) else {
            return;
        };
        let placeholder = t(cx, "Issue title").to_owned();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(title)
        });
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_title(cx);
            }
        });
        self.title_input = Some(input);
        self._title_sub = Some(sub);
        self.editing = Some(EditField::Title);
        cx.notify();
    }

    fn submit_title(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(input) = &self.title_input else {
            return;
        };
        let next = input.read(cx).value().trim().to_owned();
        let current = self.detail.as_ref().map(|d| d.title.as_str()).unwrap_or("");
        if next.is_empty() || next == current {
            self.cancel_edit(cx);
            return;
        }
        self.save_update(Some(next), None, None, None, cx);
    }

    fn start_edit_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(body) = self.detail.as_ref().map(|d| d.body.clone()) else {
            return;
        };
        let placeholder = t(cx, "Description").to_owned();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 16)
                .placeholder(placeholder)
                .default_value(body)
        });
        self.body_input = Some(input);
        self.editing = Some(EditField::Body);
        cx.notify();
    }

    fn submit_body(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(input) = &self.body_input else {
            return;
        };
        let next = input.read(cx).value().to_string();
        self.save_update(None, Some(next), None, None, cx);
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.title_input = None;
        self.body_input = None;
        self._title_sub = None;
        cx.notify();
    }

    fn save_update(
        &mut self,
        title: Option<String>,
        body: Option<String>,
        labels: Option<Vec<String>>,
        assignees: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let input = IssueUpdateInput {
            reference: self.reference.clone(),
            title,
            body,
            labels,
            assignees,
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = write_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::UPDATE,
            payload,
            Ok,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        let mutation_id = self.alloc_mutation_id();
        self.saving = true;
        let handle = cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                panel._mutations.remove(&mutation_id);
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                panel.saving = false;
                match result {
                    Ok(_) => {
                        panel.editing = None;
                        panel.title_input = None;
                        panel.body_input = None;
                        panel._title_sub = None;
                        panel.notify_mutated(cx);
                        panel.fetch_detail(true, cx);
                    }
                    Err(failure) => {
                        let title = t(cx, "The issue could not be updated").to_owned();
                        panel.state.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        });
        self._mutations.insert(mutation_id, handle);
        cx.notify();
    }

    fn toggle_reaction(
        &mut self,
        subject_id: Option<String>,
        content: IssueReactionContent,
        reacted: bool,
        cx: &mut Context<Self>,
    ) {
        let input = IssueReactionInput {
            reference: self.reference.clone(),
            subject_id,
            content,
            reacted,
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = write_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::REACT,
            payload,
            Ok,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        let mutation_id = self.alloc_mutation_id();
        let handle = cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                panel._mutations.remove(&mutation_id);
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                if result.is_ok() {
                    panel.fetch_detail(true, cx);
                }
            });
        });
        self._mutations.insert(mutation_id, handle);
    }

    fn open_candidate_picker(
        &mut self,
        kind: CandidatePickerKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rev = self.detail_revision;
        match kind {
            CandidatePickerKind::Labels => {
                self.label_picks = issue_picks_for(&self.label_picks, rev, false);
            }
            CandidatePickerKind::Assignees => {
                self.assignee_picks = issue_picks_for(&self.assignee_picks, rev, false);
            }
        }
        let placeholder = match kind {
            CandidatePickerKind::Labels => t(cx, "Search labels").to_owned(),
            CandidatePickerKind::Assignees => t(cx, "Search people").to_owned(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.picker_query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.picker = Some(kind);
        self.picker_input = Some(input);
        self.picker_query.clear();
        self._picker_sub = Some(sub);
        if self.candidates.is_none() && !self.candidates_pending {
            self.fetch_candidates(cx);
        }
        cx.notify();
    }

    fn fetch_candidates(&mut self, cx: &mut Context<Self>) {
        let scope_ref = IssueScopeRef {
            project_id: self.reference.project_id.clone(),
            tracker: self.reference.tracker.clone(),
            host: self.reference.host.clone(),
            scope: self.reference.scope.clone(),
        };
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = read_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::CANDIDATES,
            scope_ref.payload(),
            decode::<IssueCandidates>,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        self.candidates_pending = true;
        self.candidates_error = None;
        self._candidates = Some(cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                panel.candidates_pending = false;
                match result {
                    Ok(data) => {
                        panel.candidates = Some(data);
                        panel.candidates_error = None;
                    }
                    Err(failure) => {
                        panel.candidates_error = Some(failure.message);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn close_candidate_picker(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.picker.take() else {
            return;
        };
        self.picker_input = None;
        self.picker_query.clear();
        self._picker_sub = None;
        let Some(detail) = self.detail.as_ref() else {
            cx.notify();
            return;
        };
        match kind {
            CandidatePickerKind::Labels => {
                let current = issue_picks_for(&self.label_picks, self.detail_revision, true);
                let held: Vec<String> = detail.labels.iter().map(|l| l.name.clone()).collect();
                let next = issue_write_after_picks(
                    issue_labels_after_picks(&detail.labels, &current.draft),
                    &held,
                    current.written.as_deref(),
                );
                if let Some(labels) = next {
                    let mut sent = current;
                    sent.written = Some(labels.clone());
                    self.label_picks = sent.clone();
                    self.write_labels_or_assignees(Some(labels), None, Some(sent), None, cx);
                }
            }
            CandidatePickerKind::Assignees => {
                let current = issue_picks_for(&self.assignee_picks, self.detail_revision, true);
                let held: Vec<String> = detail.assignees.iter().map(|a| a.login.clone()).collect();
                let next = issue_write_after_picks(
                    issue_assignees_after_picks(&detail.assignees, &current.draft),
                    &held,
                    current.written.as_deref(),
                );
                if let Some(assignees) = next {
                    let mut sent = current;
                    sent.written = Some(assignees.clone());
                    self.assignee_picks = sent.clone();
                    self.write_labels_or_assignees(None, Some(assignees), None, Some(sent), cx);
                }
            }
        }
        cx.notify();
    }

    fn write_labels_or_assignees(
        &mut self,
        labels: Option<Vec<String>>,
        assignees: Option<Vec<String>>,
        sent_labels: Option<IssuePicks<IssueLabelCandidate>>,
        sent_assignees: Option<IssuePicks<IssueAssigneeCandidate>>,
        cx: &mut Context<Self>,
    ) {
        let is_labels = labels.is_some();
        let input = IssueUpdateInput {
            reference: self.reference.clone(),
            title: None,
            body: None,
            labels,
            assignees,
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = write_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::UPDATE,
            payload,
            Ok,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        let mutation_id = self.alloc_mutation_id();
        let handle = cx.spawn(async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update(cx, |panel, cx| {
                panel._mutations.remove(&mutation_id);
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                match result {
                    Ok(_) => {
                        panel.notify_mutated(cx);
                        panel.fetch_detail(true, cx);
                    }
                    Err(failure) => {
                        if let Some(sent) = sent_labels {
                            panel.label_picks =
                                issue_picks_after_failed_write(&panel.label_picks, &sent);
                        }
                        if let Some(sent) = sent_assignees {
                            panel.assignee_picks =
                                issue_picks_after_failed_write(&panel.assignee_picks, &sent);
                        }
                        let title = if is_labels {
                            t(cx, "Could not change labels").to_owned()
                        } else {
                            t(cx, "Could not change assignees").to_owned()
                        };
                        panel.state.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        });
        self._mutations.insert(mutation_id, handle);
    }

    fn ensure_comment_input(
        &mut self,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        if let Some(input) = &self.comment_input {
            return input.clone();
        }
        let initial = self.comment_draft.clone();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder(placeholder)
                .default_value(initial)
        });
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.comment_draft = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.comment_input = Some(input.clone());
        self._comment_sub = Some(sub);
        input
    }

    fn submit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comment_submitting {
            return;
        }
        let text = self
            .comment_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.comment_draft.clone());
        if text.trim().is_empty() {
            return;
        }
        let input = IssueCommentInput {
            reference: self.reference.clone(),
            body: text,
        };
        let payload = serde_json::to_value(input).unwrap_or_else(|_| json!({}));
        let owner = environment_owner_for(self.state.read(cx), self.environment_id.as_deref());
        let task = write_for_owner(
            self.state.read(cx),
            owner.as_ref(),
            method::COMMENT,
            payload,
            Ok,
        );
        let expected_ref = self.reference.clone();
        let expected_env = self.environment_id.clone();
        let mutation_id = self.alloc_mutation_id();
        self.comment_submitting = true;
        let handle = cx.spawn_in(window, async move |this, cx| {
            let result = settle(task).await;
            let _ = this.update_in(cx, |panel, window, cx| {
                panel._mutations.remove(&mutation_id);
                if panel.reference != expected_ref || panel.environment_id != expected_env {
                    return;
                }
                panel.comment_submitting = false;
                match result {
                    Ok(_) => {
                        panel.comment_draft.clear();
                        if let Some(input) = &panel.comment_input {
                            input.update(cx, |state, cx| state.set_value("", window, cx));
                        }
                        panel.notify_mutated(cx);
                        panel.fetch_detail(true, cx);
                    }
                    Err(failure) => {
                        let title = t(cx, "The comment could not be posted").to_owned();
                        panel.state.update(cx, |app, cx| {
                            app.show_toast(
                                Toast::new(ToastKind::Error, title).description(failure.message),
                                cx,
                            );
                        });
                    }
                }
                cx.notify();
            });
        });
        self._mutations.insert(mutation_id, handle);
        cx.notify();
    }

    fn notify_mutated(&self, cx: &mut App) {
        if let Some(handler) = &self.on_mutated {
            handler(cx);
        }
    }

    fn project_cwd(&self, cx: &App) -> Option<String> {
        let app = self.state.read(cx);
        if let Some(target_env) = self.environment_id.as_deref()
            && let Some(env) = app.fleet.environments.iter().find(|env| {
                env.summary.owner.saved_id == target_env
                    || env.summary.owner.canonical_id.as_deref() == Some(target_env)
            })
        {
            return env
                .summary
                .projects
                .iter()
                .find(|p| p.project.id == self.reference.project_id)
                .map(|p| p.project.workspace_root.clone())
                .filter(|root| !root.is_empty());
        }
        app.shell
            .project(&self.reference.project_id)
            .map(|p| p.workspace_root.clone())
            .filter(|root| !root.is_empty())
    }

    fn markdown_action_ctx(&self, cx: &Context<Self>) -> MarkdownActionContext {
        MarkdownActionContext {
            state: self.state.clone(),
            owner: cx.entity_id(),
            cwd: self.project_cwd(cx),
            thread_id: self.thread_id.clone(),
            thread_view: None,
        }
    }

    fn render_reaction_bar(
        &self,
        reactions: &[IssueReaction],
        can_react: bool,
        subject_id: Option<String>,
        id_prefix: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let pills: Vec<AnyElement> = reactions
            .iter()
            .filter(|r| r.count > 0)
            .enumerate()
            .map(|(idx, reaction)| {
                let content = reaction.content;
                let reacted = reaction.viewer_has_reacted;
                let subj = subject_id.clone();
                let id = SharedString::from(format!("{id_prefix}-react-{idx}"));
                h_flex()
                    .id(id)
                    .h(px(22.))
                    .px(px(6.))
                    .gap(px(4.))
                    .rounded_full()
                    .border_1()
                    .border_color(if reacted {
                        hsla(p.primary).opacity(0.6)
                    } else {
                        hsla(p.border)
                    })
                    .bg(if reacted {
                        hsla(p.primary).opacity(0.12)
                    } else {
                        hsla(p.card)
                    })
                    .text_xs()
                    .text_color(hsla(p.foreground))
                    .when(can_react, |this| {
                        this.cursor_pointer()
                            .hover(|style| style.bg(hsla(p.accent)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_reaction(subj.clone(), content, !reacted, cx);
                            }))
                    })
                    .child(reaction_emoji(content))
                    .child(reaction.count.to_string())
                    .into_any_element()
            })
            .collect();

        let add_menu = can_react.then(|| {
            let entity = cx.entity();
            let existing: Vec<(IssueReactionContent, bool)> = reactions
                .iter()
                .map(|r| (r.content, r.viewer_has_reacted))
                .collect();
            let subj = subject_id.clone();
            Button::new(
                SharedString::from(format!("{id_prefix}-react-add")),
                Variant::GhostMuted,
                Size::Xs,
            )
            .icon(Icon::Plus)
            .tooltip(t(cx, "Add reaction").to_owned())
            .dropdown_menu(move |mut menu, _, _| {
                for content in REACTION_ORDER {
                    let reacted = existing
                        .iter()
                        .find(|(c, _)| *c == content)
                        .is_some_and(|(_, r)| *r);
                    let entity = entity.clone();
                    let subj = subj.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!(
                            "{} {}",
                            reaction_emoji(content),
                            reaction_name(content)
                        ))
                        .checked(reacted)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.toggle_reaction(subj.clone(), content, !reacted, cx);
                            });
                        }),
                    );
                }
                menu
            })
        });

        h_flex()
            .gap(px(4.))
            .flex_wrap()
            .items_center()
            .children(pills)
            .children(add_menu)
            .into_any_element()
    }

    fn render_linked_pr_row(
        &self,
        index: usize,
        link: &IssueLinkedPullRequest,
        scope: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = palette(cx);
        let kind = PrStateKind::of(link.state, link.is_draft);
        let (icon, color) = pr_state_glyph(kind, cx);
        let number_label = if link.repository.eq_ignore_ascii_case(scope) {
            format!("#{}", link.number)
        } else {
            format!("{}#{}", link.repository, link.number)
        };
        let on_select = self.on_select_pr.clone();
        let pr_ref = PrRef {
            project_id: self.reference.project_id.clone(),
            host: self.reference.host.clone(),
            repository: link.repository.clone(),
            number: link.number,
        };
        let fallback_url = link.url.clone();

        h_flex()
            .id(SharedString::from(format!("issue-linked-pr-{index}")))
            .w_full()
            .min_w_0()
            .px(px(8.))
            .py(px(6.))
            .gap(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(hsla(p.accent)))
            .on_click(move |_, _, cx| {
                if let Some(handler) = &on_select {
                    handler(pr_ref.clone(), cx);
                } else {
                    cx.open_url(&fallback_url);
                }
            })
            .child(icon.sized(px(14.)).text_color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(hsla(p.foreground))
                    .child(link.title.clone()),
            )
            .when(link.closes_issue, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(t(cx, "Closes this issue").to_owned()),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(number_label),
            )
            .into_any_element()
    }

    fn render_candidate_picker_overlay(
        &self,
        detail: &IssueDetail,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let kind = self.picker?;
        let input = self.picker_input.as_ref()?.clone();
        let p = palette(cx);
        let needle = self.picker_query.trim().to_lowercase();
        let rev = self.detail_revision;
        let title = match kind {
            CandidatePickerKind::Labels => t(cx, "Change labels").to_owned(),
            CandidatePickerKind::Assignees => t(cx, "Change assignees").to_owned(),
        };

        let body: AnyElement = if self.candidates_pending && self.candidates.is_none() {
            div()
                .px(px(12.))
                .py(px(16.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(t(cx, "Loading…").to_owned())
                .into_any_element()
        } else if let Some(err) = self.candidates_error.as_ref().filter(|_| self.candidates.is_none()) {
            div()
                .px(px(12.))
                .py(px(12.))
                .text_xs()
                .text_color(hsla(p.destructive_foreground))
                .child(err.clone())
                .into_any_element()
        } else {
            let candidates = self.candidates.as_ref();
            let truncated = candidates.is_some_and(|c| c.truncated);
            let rows: Vec<AnyElement> = match kind {
                CandidatePickerKind::Labels => {
                    let picks = issue_picks_for(&self.label_picks, rev, true);
                    let list: Vec<IssueLabelCandidate> = candidates
                        .map(|c| c.labels.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|label| {
                            needle.is_empty()
                                || label.name.to_lowercase().contains(&needle)
                                || label
                                    .description
                                    .as_deref()
                                    .unwrap_or("")
                                    .to_lowercase()
                                    .contains(&needle)
                        })
                        .collect();
                    if list.is_empty() {
                        vec![
                            div()
                                .px(px(10.))
                                .py(px(12.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(if needle.is_empty() {
                                    t(cx, "This tracker has no labels.").to_owned()
                                } else {
                                    t(cx, "No label matches that.").to_owned()
                                })
                                .into_any_element(),
                        ]
                    } else {
                        list.into_iter()
                            .enumerate()
                            .map(|(idx, candidate)| {
                                let held = detail.labels.iter().any(|l| l.name == candidate.name);
                                let selected = picks
                                    .draft
                                    .get(&candidate.name)
                                    .map_or(held, |entry| entry.selected);
                                let dot_color = label_color(candidate.color.as_deref())
                                    .map(gpui::rgb)
                                    .map(gpui::Hsla::from)
                                    .unwrap_or_else(|| hsla(p.muted_foreground));
                                let cand_click = candidate.clone();
                                h_flex()
                                    .id(SharedString::from(format!("issue-label-cand-{idx}")))
                                    .w_full()
                                    .px(px(8.))
                                    .py(px(6.))
                                    .gap(px(8.))
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(hsla(p.accent)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let mut cur = issue_picks_for(
                                            &this.label_picks,
                                            this.detail_revision,
                                            true,
                                        );
                                        let now_selected = !selected;
                                        if now_selected == held {
                                            cur.draft.remove(&cand_click.name);
                                        } else {
                                            cur.draft.insert(
                                                cand_click.name.clone(),
                                                SelectionEntry {
                                                    selected: now_selected,
                                                    item: cand_click.clone(),
                                                },
                                            );
                                        }
                                        this.label_picks = cur;
                                        cx.notify();
                                    }))
                                    .child(div().size(px(8.)).rounded_full().bg(dot_color))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(hsla(p.foreground))
                                            .child(candidate.name.clone()),
                                    )
                                    .when(selected, |this| {
                                        this.child(
                                            Icon::Check
                                                .sized(px(14.))
                                                .text_color(hsla(p.foreground)),
                                        )
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                }
                CandidatePickerKind::Assignees => {
                    let picks = issue_picks_for(&self.assignee_picks, rev, true);
                    let list: Vec<IssueAssigneeCandidate> = candidates
                        .map(|c| c.assignees.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|actor| {
                            needle.is_empty()
                                || actor.login.to_lowercase().contains(&needle)
                                || actor
                                    .name
                                    .as_deref()
                                    .unwrap_or("")
                                    .to_lowercase()
                                    .contains(&needle)
                        })
                        .collect();
                    if list.is_empty() {
                        vec![
                            div()
                                .px(px(10.))
                                .py(px(12.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(if needle.is_empty() {
                                    t(cx, "Nobody can be assigned here.").to_owned()
                                } else {
                                    t(cx, "Nobody matches that.").to_owned()
                                })
                                .into_any_element(),
                        ]
                    } else {
                        list.into_iter()
                            .enumerate()
                            .map(|(idx, candidate)| {
                                let held = issue_assignee_held(&detail.assignees, &candidate);
                                let selected = picks
                                    .draft
                                    .get(&candidate.id)
                                    .map_or(held, |entry| entry.selected);
                                let cand_click = candidate.clone();
                                h_flex()
                                    .id(SharedString::from(format!("issue-assignee-cand-{idx}")))
                                    .w_full()
                                    .px(px(8.))
                                    .py(px(6.))
                                    .gap(px(8.))
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(hsla(p.accent)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let mut cur = issue_picks_for(
                                            &this.assignee_picks,
                                            this.detail_revision,
                                            true,
                                        );
                                        let now_selected = !selected;
                                        if now_selected == held {
                                            cur.draft.remove(&cand_click.id);
                                        } else {
                                            cur.draft.insert(
                                                cand_click.id.clone(),
                                                SelectionEntry {
                                                    selected: now_selected,
                                                    item: cand_click.clone(),
                                                },
                                            );
                                        }
                                        this.assignee_picks = cur;
                                        cx.notify();
                                    }))
                                    .child(avatar(&candidate.login, 16., cx))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(hsla(p.foreground))
                                            .child(candidate.login.clone()),
                                    )
                                    .when(selected, |this| {
                                        this.child(
                                            Icon::Check
                                                .sized(px(14.))
                                                .text_color(hsla(p.foreground)),
                                        )
                                    })
                                    .into_any_element()
                            })
                            .collect()
                    }
                }
            };
            v_flex()
                .gap(px(2.))
                .children(rows)
                .when(truncated, |this| {
                    this.child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .child(match kind {
                                CandidatePickerKind::Labels => {
                                    t(cx, "More labels exist than are listed here.").to_owned()
                                }
                                CandidatePickerKind::Assignees => {
                                    t(cx, "More people exist than are listed here.").to_owned()
                                }
                            }),
                    )
                })
                .into_any_element()
        };

        Some(
            div()
                .id("issue-candidate-picker-backdrop")
                .absolute()
                .inset_0()
                .bg(hsla(p.background).opacity(0.55))
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_candidate_picker(cx)),
                )
                .child(
                    v_flex()
                        .id("issue-candidate-picker-dialog")
                        .w(px(340.))
                        .max_w_full()
                        .max_h(px(380.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.popover))
                        .shadow_lg()
                        .p(px(12.))
                        .gap(px(8.))
                        .occlude()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.foreground))
                                        .child(title),
                                )
                                .child(
                                    Button::new(
                                        "issue-candidate-picker-done",
                                        Variant::Outline,
                                        Size::Xs,
                                    )
                                    .label(t(cx, "Done").to_owned())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_candidate_picker(cx);
                                    })),
                                ),
                        )
                        .child(Input::new(&input))
                        .child(
                            div()
                                .id("issue-candidate-picker-list")
                                .flex_1()
                                .min_h_0()
                                .max_h(px(260.))
                                .overflow_y_scroll()
                                .child(body),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for IssueDetailPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.markdown.begin_frame();
        let p = palette(cx);

        let Some(detail) = self.detail.clone() else {
            if let Some(error) = self.error.clone().filter(|_| !self.pending) {
                let retry = cx.listener(|this, _, _, cx| this.fetch_detail(false, cx));
                return render_unavailable(&error, self.pending, retry, cx);
            }
            return render_ghost(self.seed.as_ref(), cx);
        };

        let can_write = can_write_issues(self.state.read(cx), self.environment_id.as_deref());
        let access = issue_write_access(&detail, can_write);
        let pager_view = self.pager.view(Some(&detail), self.pending);
        let timeline = merge_issue_timeline(&pager_view.comments, &detail.events);
        let (state_icon, state_color) = state_glyph(&detail.state, cx);
        let open_url = detail.url.clone();

        // Tracker-native state transition control (`pingdotgg/t3code#37930`).
        let state_control: AnyElement = if !access.states.is_empty() {
            let entity = cx.entity();
            let other_states = access.states.clone();
            Button::new("issue-state-menu", Variant::Outline, Size::Xs)
                .icon(state_icon)
                .label(detail.state.name.clone())
                .disabled(self.changing_state)
                .dropdown_menu(move |mut menu, _, _| {
                    for state in &other_states {
                        let entity = entity.clone();
                        let name = state.name.clone();
                        menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                            move |_, _, cx| {
                                entity.update(cx, |this, cx| this.move_to_state(name.clone(), cx));
                            },
                        ));
                    }
                    menu
                })
                .into_any_element()
        } else {
            h_flex()
                .flex_none()
                .gap(px(4.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(state_color)
                .child(state_icon.sized(px(14.)))
                .child(detail.state.name.clone())
                .into_any_element()
        };

        let thread_links_controls = thread_links::render_header_controls(
            &self.thread_links,
            &self.state,
            self.environment_id.clone(),
            &self.reference,
            &detail,
            self.thread_id.as_deref(),
            |this| &mut this.thread_links,
            cx,
        );

        let close_btn = self.on_close.clone().map(|handler| {
            Button::new("issue-detail-close", Variant::GhostMuted, Size::IconSm)
                .icon(Icon::X)
                .tooltip(t(cx, "Close").to_owned())
                .on_click(move |_, _, cx| handler(cx))
        });

        let refresh_btn = Button::new("issue-detail-refresh", Variant::GhostMuted, Size::IconSm)
            .icon(Icon::RefreshCw)
            .tooltip(t(cx, "Refresh issue").to_owned())
            .busy(self.pending)
            .disabled(self.pending)
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)));

        // Refresh error banner (`pingdotgg/t3code#38537`) shown alongside preserved detail/comments.
        let refresh_error_banner = self
            .error
            .as_ref()
            .filter(|_| !self.pending)
            .map(|error| {
                v_flex()
                    .id("issue-refresh-error")
                    .mt(px(8.))
                    .px(px(12.))
                    .py(px(10.))
                    .gap(px(4.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.destructive).opacity(0.4))
                    .bg(hsla(p.destructive).opacity(0.1))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.destructive_foreground))
                            .child(t(cx, "Could not refresh this issue").to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hsla(p.foreground))
                            .child(error.clone()),
                    )
            });

        // Title block or inline title editor.
        let title_block: AnyElement = if self.editing == Some(EditField::Title)
            && let Some(input) = self.title_input.clone()
        {
            h_flex()
                .mt(px(6.))
                .gap(px(8.))
                .items_center()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_edit(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(div().flex_1().min_w_0().child(Input::new(&input).disabled(self.saving)))
                .child(
                    Button::new("issue-title-save", Variant::Default, Size::Sm)
                        .label(t(cx, "Save").to_owned())
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.submit_title(cx))),
                )
                .child(
                    Button::new("issue-title-cancel", Variant::Outline, Size::Sm)
                        .label(t(cx, "Cancel").to_owned())
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                )
                .into_any_element()
        } else {
            h_flex()
                .mt(px(6.))
                .gap(px(6.))
                .items_start()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_height(px(22.))
                        .text_color(hsla(p.foreground))
                        .child(detail.title.clone()),
                )
                .when(access.edit, |this| {
                    this.child(
                        Button::new("issue-title-edit", Variant::GhostMuted, Size::IconSm)
                            .icon(Icon::Pencil)
                            .tooltip(t(cx, "Edit title").to_owned())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_edit_title(window, cx);
                            })),
                    )
                })
                .into_any_element()
        };

        let author_login = detail
            .author
            .as_ref()
            .map_or("ghost", |actor| actor.login.as_str())
            .to_owned();
        let opened = parse_iso(&detail.created_at)
            .map(|at| compact_relative_label(at, now_ms()))
            .unwrap_or_default();
        let closed = detail
            .closed_at
            .as_deref()
            .and_then(parse_iso)
            .map(|at| compact_relative_label(at, now_ms()));

        let labels_disabled_reason = access
            .blocked
            .clone()
            .unwrap_or_else(|| t(cx, "Changing labels needs triage access on this tracker.").to_owned());
        let assignees_disabled_reason = access
            .blocked
            .clone()
            .unwrap_or_else(|| t(cx, "Changing assignees needs triage access on this tracker.").to_owned());

        let action_ctx = self.markdown_action_ctx(cx);
        let body_text: Arc<str> = if detail.body.trim().is_empty() {
            Arc::from(t(cx, "_No description provided._"))
        } else {
            Arc::from(detail.body.as_str())
        };
        let body_md = self.markdown.state_in_context(
            &format!("issue-body:{}", detail.url),
            &body_text,
            Some(action_ctx.clone()),
            cx,
        );
        let description_block: AnyElement = if self.editing == Some(EditField::Body)
            && let Some(input) = self.body_input.clone()
        {
            v_flex()
                .gap(px(8.))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_edit(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(Textarea::new(&input).disabled(self.saving))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .child(
                            Button::new("issue-body-cancel", Variant::Outline, Size::Sm)
                                .label(t(cx, "Cancel").to_owned())
                                .disabled(self.saving)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_edit(cx))),
                        )
                        .child(
                            Button::new("issue-body-save", Variant::Default, Size::Sm)
                                .label(t(cx, "Save").to_owned())
                                .disabled(self.saving)
                                .on_click(cx.listener(|this, _, _, cx| this.submit_body(cx))),
                        ),
                )
                .into_any_element()
        } else {
            prose(&body_md, cx).into_any_element()
        };

        let body_reactions = detail.reactions.as_ref().map(|reactions| {
            self.render_reaction_bar(reactions, access.react, None, "issue-body", cx)
        });

        let linked_prs_section = (!detail.linked_pull_requests.is_empty()).then(|| {
            let rows: Vec<AnyElement> = detail
                .linked_pull_requests
                .iter()
                .enumerate()
                .map(|(idx, link)| self.render_linked_pr_row(idx, link, &detail.scope, cx))
                .collect();
            v_flex()
                .pt(px(16.))
                .border_t_1()
                .border_color(hsla(p.border).opacity(0.6))
                .gap(px(8.))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.muted_foreground))
                        .child(t(cx, "Linked pull requests").to_owned()),
                )
                .child(v_flex().gap(px(2.)).children(rows))
        });

        let activity_title = if detail.comment_count > 0 {
            format!("{} ({})", t(cx, "Activity"), detail.comment_count)
        } else {
            t(cx, "Activity").to_owned()
        };
        let load_older_btn = pager_view.next_cursor.is_some().then(|| {
            Button::new("issue-load-older-comments", Variant::Outline, Size::Sm)
                .label(if pager_view.loading_older {
                    t(cx, "Loading…").to_owned()
                } else {
                    t(cx, "Load older comments").to_owned()
                })
                .disabled(!pager_view.can_load_older)
                .on_click(cx.listener(|this, _, _, cx| this.load_older_comments(cx)))
        });

        let timeline_elements: Vec<AnyElement> = timeline
            .iter()
            .enumerate()
            .map(|(idx, item)| match item {
                IssueTimelineItem::Comment(comment) => {
                    let c_author = comment
                        .author
                        .as_ref()
                        .map_or("ghost", |a| a.login.as_str())
                        .to_owned();
                    let when = parse_iso(&comment.created_at)
                        .map(|at| compact_relative_label(at, now_ms()))
                        .unwrap_or_default();
                    let c_url = comment.url.clone();
                    let c_text: Arc<str> = Arc::from(comment.body.as_str());
                    let c_md = self.markdown.state_in_context(
                        &format!("issue-comment:{}:{}", detail.url, comment.id),
                        &c_text,
                        Some(action_ctx.clone()),
                        cx,
                    );
                    let c_reactions = comment.reactions.as_ref().map(|reactions| {
                        self.render_reaction_bar(
                            reactions,
                            access.react,
                            Some(comment.id.clone()),
                            &format!("issue-c-{idx}"),
                            cx,
                        )
                    });
                    v_flex()
                        .id(SharedString::from(format!("issue-comment-{idx}")))
                        .gap(px(8.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(hsla(p.border).opacity(0.65))
                        .p(px(12.))
                        .child(
                            h_flex()
                                .gap(px(6.))
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(avatar(&c_author, 16., cx))
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(hsla(p.foreground))
                                        .child(c_author),
                                )
                                .child(if let Some(url) = c_url {
                                    div()
                                        .id(SharedString::from(format!("issue-comment-time-{idx}")))
                                        .cursor_pointer()
                                        .hover(|s| s.text_color(hsla(p.foreground)))
                                        .on_click(move |_, _, cx| cx.open_url(&url))
                                        .child(when)
                                        .into_any_element()
                                } else {
                                    div().child(when).into_any_element()
                                }),
                        )
                        .child(prose(&c_md, cx))
                        .children(c_reactions)
                        .into_any_element()
                }
                IssueTimelineItem::Event(event) => {
                    let e_actor = event
                        .actor
                        .as_ref()
                        .map_or("ghost", |a| a.login.as_str())
                        .to_owned();
                    let sentence = issue_event_sentence(event);
                    let when = parse_iso(&event.created_at)
                        .map(|at| compact_relative_label(at, now_ms()))
                        .unwrap_or_default();
                    h_flex()
                        .id(SharedString::from(format!("issue-event-{idx}")))
                        .px(px(4.))
                        .gap(px(6.))
                        .flex_wrap()
                        .text_xs()
                        .text_color(hsla(p.muted_foreground))
                        .child(avatar(&e_actor, 16., cx))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child(e_actor),
                        )
                        .child(sentence)
                        .child(when)
                        .into_any_element()
                }
            })
            .collect();

        // Bottom comment composer (`IssueCommentComposer.tsx`).
        let composer = detail.capabilities.comment.then(|| {
            let blocked_reason = if access.comment {
                None
            } else {
                Some(
                    access
                        .blocked
                        .clone()
                        .unwrap_or_else(|| t(cx, "You cannot comment here.").to_owned()),
                )
            };
            let locked = blocked_reason.is_some() || self.comment_submitting;
            let placeholder = blocked_reason.unwrap_or_else(|| t(cx, "Leave a comment").to_owned());
            let input = self.ensure_comment_input(placeholder, window, cx);
            let submit_disabled = locked || self.comment_draft.trim().is_empty();
            div()
                .id("issue-comment-composer")
                .flex_none()
                .border_t_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.background))
                .p(px(12.))
                .child(
                    v_flex()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.card))
                        .p(px(8.))
                        .gap(px(6.))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "enter"
                                && (event.keystroke.modifiers.platform
                                    || event.keystroke.modifiers.control)
                                && !event.keystroke.modifiers.shift
                                && !locked
                            {
                                cx.stop_propagation();
                                this.submit_comment(window, cx);
                            }
                        }))
                        .child(Textarea::new(&input).disabled(locked))
                        .child(
                            h_flex().justify_end().child(
                                Button::new(
                                    "issue-comment-submit",
                                    Variant::Default,
                                    Size::IconSm,
                                )
                                .icon(Icon::ArrowUp)
                                .busy(self.comment_submitting)
                                .disabled(submit_disabled)
                                .tooltip(if self.comment_submitting {
                                    t(cx, "Posting comment").to_owned()
                                } else {
                                    t(cx, "Comment").to_owned()
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_comment(window, cx);
                                })),
                            ),
                        ),
                )
        });

        let thread_picker_overlay = thread_links::render_picker_overlay(
            &self.thread_links,
            &self.state,
            self.environment_id.clone(),
            &self.reference,
            &detail,
            |this| &mut this.thread_links,
            cx,
        );
        let candidate_picker_overlay = self.render_candidate_picker_overlay(&detail, cx);

        div()
            .id("issue-detail-panel")
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(hsla(p.background))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if this.thread_links.picker_open {
                        this.thread_links.close_picker();
                        cx.stop_propagation();
                        cx.notify();
                    } else if this.picker.is_some() {
                        this.close_candidate_picker(cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(
                div()
                    .id("issue-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .pt(px(12.))
                    .pb(px(24.))
                    .child(
                        v_flex()
                            .gap(px(12.))
                            .child(
                                h_flex()
                                    .h(px(28.))
                                    .gap(px(6.))
                                    .items_center()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.muted_foreground))
                                            .child(detail.scope.clone()),
                                    )
                                    .child(
                                        h_flex()
                                            .id("issue-detail-key-link")
                                            .flex_none()
                                            .gap(px(3.))
                                            .cursor_pointer()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(state_color)
                                            .on_click(move |_, _, cx| cx.open_url(&open_url))
                                            .child(detail.display_key.clone())
                                            .child(Icon::ExternalLink.sized(px(11.))),
                                    )
                                    .child(div().flex_1())
                                    .child(state_control)
                                    .child(thread_links_controls)
                                    .child(refresh_btn)
                                    .children(close_btn),
                            )
                            .children(refresh_error_banner)
                            .child(title_block)
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .flex_wrap()
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(avatar(&author_login, 16., cx))
                                    .child(author_login)
                                    .child(format!("{} {opened}", t(cx, "opened")))
                                    .children(
                                        closed.map(|when| format!("· {} {when}", t(cx, "closed"))),
                                    )
                                    .when(!detail.writable, |this| {
                                        this.child(format!("· {}", t(cx, "Read-only")))
                                    }),
                            )
                            .child(
                                v_flex()
                                    .gap(px(8.))
                                    .pt(px(4.))
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .items_center()
                                            .child(
                                                h_flex()
                                                    .w(px(84.))
                                                    .flex_none()
                                                    .gap(px(6.))
                                                    .text_xs()
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(Icon::Tag.sized(px(13.)))
                                                    .child(t(cx, "Labels").to_owned()),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_wrap()
                                                    .gap(px(4.))
                                                    .items_center()
                                                    .when(detail.labels.is_empty(), |this| {
                                                        this.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(hsla(
                                                                    p.muted_foreground,
                                                                ))
                                                                .child(t(cx, "None").to_owned()),
                                                        )
                                                    })
                                                    .children(detail.labels.iter().map(|label| {
                                                        let kind =
                                                            label_color(label.color.as_deref())
                                                                .map_or(
                                                                    BadgeKind::Secondary,
                                                                    BadgeKind::Label,
                                                                );
                                                        badge(label.name.clone(), kind, false, cx)
                                                    }))
                                                    .when(detail.capabilities.labels, |this| {
                                                        this.child(
                                                            Button::new(
                                                                "issue-edit-labels",
                                                                Variant::GhostMuted,
                                                                Size::Xs,
                                                            )
                                                            .icon(Icon::Tag)
                                                            .disabled(!access.labels)
                                                            .tooltip(if access.labels {
                                                                t(cx, "Change labels").to_owned()
                                                            } else {
                                                                labels_disabled_reason.clone()
                                                            })
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.open_candidate_picker(
                                                                        CandidatePickerKind::Labels,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                        )
                                                    }),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .items_center()
                                            .child(
                                                h_flex()
                                                    .w(px(84.))
                                                    .flex_none()
                                                    .gap(px(6.))
                                                    .text_xs()
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(Icon::Users.sized(px(13.)))
                                                    .child(t(cx, "Assignees").to_owned()),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_wrap()
                                                    .gap(px(6.))
                                                    .items_center()
                                                    .when(detail.assignees.is_empty(), |this| {
                                                        this.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(hsla(
                                                                    p.muted_foreground,
                                                                ))
                                                                .child(t(cx, "None").to_owned()),
                                                        )
                                                    })
                                                    .children(detail.assignees.iter().map(
                                                        |actor| {
                                                            h_flex()
                                                                .gap(px(4.))
                                                                .text_xs()
                                                                .text_color(hsla(p.foreground))
                                                                .child(avatar(&actor.login, 16., cx))
                                                                .child(actor.login.clone())
                                                        },
                                                    ))
                                                    .when(detail.capabilities.assignees, |this| {
                                                        this.child(
                                                            Button::new(
                                                                "issue-edit-assignees",
                                                                Variant::GhostMuted,
                                                                Size::Xs,
                                                            )
                                                            .icon(Icon::UserPlus)
                                                            .disabled(!access.assignees)
                                                            .tooltip(if access.assignees {
                                                                t(cx, "Change assignees").to_owned()
                                                            } else {
                                                                assignees_disabled_reason.clone()
                                                            })
                                                            .on_click(cx.listener(
                                                                |this, _, window, cx| {
                                                                    this.open_candidate_picker(
                                                                        CandidatePickerKind::Assignees,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                        )
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .pt(px(14.))
                                    .border_t_1()
                                    .border_color(hsla(p.border).opacity(0.6))
                                    .gap(px(8.))
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(hsla(p.muted_foreground))
                                                    .child(t(cx, "Description").to_owned()),
                                            )
                                            .when(
                                                access.edit && self.editing != Some(EditField::Body),
                                                |this| {
                                                    this.child(
                                                        Button::new(
                                                            "issue-body-edit",
                                                            Variant::GhostMuted,
                                                            Size::IconSm,
                                                        )
                                                        .icon(Icon::Pencil)
                                                        .tooltip(t(cx, "Edit description").to_owned())
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.start_edit_body(window, cx);
                                                            },
                                                        )),
                                                    )
                                                },
                                            ),
                                    )
                                    .child(description_block)
                                    .children(body_reactions),
                            )
                            .children(linked_prs_section)
                            .child(
                                v_flex()
                                    .pt(px(14.))
                                    .border_t_1()
                                    .border_color(hsla(p.border).opacity(0.6))
                                    .gap(px(10.))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(hsla(p.muted_foreground))
                                            .child(activity_title),
                                    )
                                    .children(load_older_btn)
                                    .when(timeline_elements.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_sm()
                                                .text_color(hsla(p.muted_foreground))
                                                .child(
                                                    t(cx, "Nothing has happened here yet.")
                                                        .to_owned(),
                                                ),
                                        )
                                    })
                                    .children(timeline_elements),
                            ),
                    ),
            )
            .children(composer)
            .children(thread_picker_overlay)
            .children(candidate_picker_overlay)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::json;
    use t3_core::issues::wire::{
        IssueComment, IssueCommentsPage, IssueDetail, IssueListEntry, IssueRef, IssueTrackerKind,
        decode,
    };

    use super::*;
    use crate::issues::ReadFailure;

    fn test_runtime() -> tokio::runtime::Handle {
        static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test tokio runtime")
        })
        .handle()
        .clone()
    }

    fn sample_ref() -> IssueRef {
        IssueRef {
            project_id: "p1".into(),
            tracker: IssueTrackerKind::GitHub,
            host: Some("github.com".into()),
            scope: "acme/web".into(),
            key: "42".into(),
        }
    }

    fn sample_detail() -> IssueDetail {
        decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "42",
            "displayKey": "#42",
            "title": "Detail panel bug",
            "url": "https://github.com/acme/web/issues/42",
            "author": { "login": "alice" },
            "state": { "category": "open", "name": "Open" },
            "states": [
                { "category": "open", "name": "Open" },
                { "category": "closed", "name": "Closed as completed", "closeReason": "completed" },
                { "category": "closed", "name": "Closed as not planned", "closeReason": "not_planned" }
            ],
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Web",
            "writable": true,
            "capabilities": {
                "create": true, "edit": true, "comment": true, "labels": true,
                "assignees": true, "reactions": true, "templates": false,
                "linkedPullRequests": true, "events": true, "search": true
            },
            "viewerPermissions": {
                "setState": true, "comment": true, "edit": true, "labels": true, "assignees": true
            },
            "body": "Initial body",
            "comments": [{
                "id": "c2",
                "author": { "login": "bob" },
                "body": "Newest comment",
                "createdAt": "2026-01-03T00:00:00Z"
            }],
            "commentsCursor": "cursor-1",
            "commentCount": 2,
            "events": [{
                "id": "e1",
                "kind": "labeled",
                "actor": { "login": "alice" },
                "createdAt": "2026-01-02T12:00:00Z",
                "detail": "bug"
            }],
            "linkedPullRequests": []
        }))
        .unwrap()
    }

    #[gpui::test]
    fn issue_detail_panel_preserves_older_comments_on_refresh_error_and_clears_on_success(
        cx: &mut gpui::TestAppContext,
    ) {
        let handle = test_runtime();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(handle.clone(), cx));
            state.update(cx, |state, _| {
                state.environments.active = Some("env-a".into());
            });
            state
        });
        let seed: IssueListEntry = decode(json!({
            "tracker": "github",
            "host": "github.com",
            "scope": "acme/web",
            "key": "42",
            "displayKey": "#42",
            "title": "Seeded title",
            "url": "https://github.com/acme/web/issues/42",
            "state": { "category": "open", "name": "Open" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "projectId": "p1",
            "projectTitle": "Web"
        }))
        .unwrap();

        let (panel, cx) = cx.add_window_view(|_, cx| {
            IssueDetailPanel::new(
                state.clone(),
                Some("env-a".into()),
                sample_ref(),
                None,
                Some(seed),
                None,
                None,
                None,
                cx,
            )
        });

        panel.update(cx, |panel, cx| {
            // Load initial detail.
            let detail = sample_detail();
            panel.apply_detail_result(false, Ok(detail.clone()), cx);
            assert_eq!(panel.detail.as_ref().map(|d| d.title.as_str()), Some("Detail panel bug"));
            assert!(panel.error.is_none());

            // Simulate loading an older comment page.
            let req = panel
                .pager
                .begin_load_older(panel.detail.as_ref(), false)
                .unwrap();
            panel.pager.finish_load_older(
                &req,
                Ok(IssueCommentsPage {
                    comments: vec![IssueComment {
                        id: "c1".into(),
                        author: None,
                        body: "Older comment".into(),
                        created_at: "2026-01-01T12:00:00Z".into(),
                        url: None,
                        reactions: None,
                    }],
                    next_cursor: None,
                }),
            );
            assert_eq!(panel.pager.view(panel.detail.as_ref(), false).comments.len(), 2);

            // Failed refresh surfaces error AND keeps loaded older comments (`#38537`, `#39000`).
            assert!(panel.pager.begin_refresh());
            panel.apply_detail_result(
                true,
                Err(ReadFailure {
                    message: "GitHub API rate limit exceeded".into(),
                    unsupported: false,
                }),
                cx,
            );
            assert_eq!(panel.error.as_deref(), Some("GitHub API rate limit exceeded"));
            assert_eq!(panel.pager.view(panel.detail.as_ref(), false).comments.len(), 2);

            // Successful refresh clears error AND resets older pages (`#38489`).
            assert!(panel.pager.begin_refresh());
            panel.apply_detail_result(true, Ok(detail), cx);
            assert!(panel.error.is_none());
            assert_eq!(panel.pager.view(panel.detail.as_ref(), false).comments.len(), 1);

            // Retargeting mid-mutation resets in-flight flags & pending mutation tasks.
            panel.candidates_pending = true;
            panel.saving = true;
            panel.changing_state = true;
            panel.comment_submitting = true;
            let mut next_ref = sample_ref();
            next_ref.key = "43".into();
            panel.retarget(Some("env-a".into()), next_ref.clone(), None, cx);
            assert!(panel.matches_target(Some("env-a"), &next_ref));
            assert!(!panel.candidates_pending);
            assert!(!panel.saving);
            assert!(!panel.changing_state);
            assert!(!panel.comment_submitting);
            assert!(panel._mutations.is_empty());
        });
    }
}
