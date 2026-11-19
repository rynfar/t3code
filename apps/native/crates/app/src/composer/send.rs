//! What Send does (`useComposerSend.ts`): launch a draft as a new thread (`startThreadTurn` with
//! `bootstrap.createThread`, in a worktree when the project's default asks for one), or send to a
//! thread after declining what it waits on and persisting permissions and mode, each step after
//! the previous one committed, as `declinePendingApprovals` and `persistThreadSettingsForNextTurn`
//! await them.

use gpui::{App, Context, EventEmitter, Window};
use serde_json::Value;
use t3_core::commands::{self, ApprovalDecision, ComposerMessage, LaunchThread, method};
use t3_core::composer::attachments::{self, Attachment};
use t3_core::composer::context::{ExpiredNotice, TerminalContext, TerminalSend, expired_toast_copy};
use t3_core::composer::menu::ThreadContext;
use t3_core::composer::{self as rules, FollowUpBehavior, SubmitIntent};
use t3_core::model::ModelSelection;
use t3_core::server_config::ThreadEnvMode;
use t3_core::traits;
use t3_core::workspace::{self, VcsRefs};

use super::{Composer, Resolved, Target};
use crate::app_state::uuid_v4;
use crate::toast::{Toast, ToastKind};

const NO_PROVIDER: &str = "Enable a provider in Settings to send a message";

async fn with_dispatch_timeout<T>(
    executor: &gpui::BackgroundExecutor,
    fut: impl std::future::Future<Output = T>,
) -> Result<T, &'static str> {
    let timer = executor.timer(std::time::Duration::from_millis(rules::MESSAGE_DISPATCH_TIMEOUT_MS));
    futures::pin_mut!(fut, timer);
    match futures::future::select(fut, timer).await {
        futures::future::Either::Left((value, _)) => Ok(value),
        futures::future::Either::Right((_, _)) => Err(rules::MESSAGE_DISPATCH_TIMEOUT_MSG),
    }
}

/// What a send took from the composer, put back if it fails.
pub(super) struct Taken {
    target: Target,
    environment: Option<String>,
    draft: t3_core::composer::drafts::Draft,
    attachments: Vec<super::attachments::Staged>,
    threads: Vec<ThreadContext>,
    records: Vec<std::sync::Arc<Value>>,
    terminals: Vec<TerminalContext>,
    reviews: Vec<t3_core::composer::review::ReviewComment>,
    reply_to: Option<Value>,
}

/// What one send carries, resolved before the prompt clears.
pub(super) struct Outgoing {
    /// The prompt as typed, put back if the send fails.
    pub(super) prompt: String,
    /// What the message says: the prompt, or the attachment-only prompt.
    pub(super) text: String,
    pub(super) title: String,
    pub(super) context: Option<Value>,
    pub(super) attachments: Vec<Value>,
}

/// Emitted by `Composer` after a `composer.sendAndNewThread` send to an existing server thread
/// succeeds while the user is still viewing that thread (`useComposerSend.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendAndNewThreadCompleted {
    pub thread_id: String,
}

impl EventEmitter<SendAndNewThreadCompleted> for Composer {}

impl Composer {
    pub(crate) fn send_alternate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.running && !matches!(self.snapshot.target, Some(Target::Draft(_))) {
            self.submit_with_dictation(SubmitIntent::Alternate, window, cx);
        }
    }

    /// `composer.sendBackground`: starts a draft thread in the background and keeps the draft open.
    pub(crate) fn send_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fixed.is_none() && matches!(self.snapshot.target, Some(Target::Draft(_))) {
            self.submit_with_dictation(SubmitIntent::Background, window, cx);
        }
    }

    /// `composer.sendAndNewThread`: sends to the open server thread and opens a fresh draft only
    /// after dispatch succeeds.
    pub(crate) fn send_and_new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fixed.is_none() && matches!(self.snapshot.target, Some(Target::Thread(_))) {
            self.submit_with_dictation(SubmitIntent::Background, window, cx);
        }
    }

    pub(crate) fn submit_with_dictation(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Submit, as the send button and Enter do.
    pub(crate) fn submit(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if let Some(audit) = cx.try_global::<super::SnapshotAudit>() {
            audit.submissions.set(audit.submissions.get() + 1);
            return;
        }
        if !self.close_citation_comment("outside-press", window, cx) {
            return;
        }
        if self.sending || self.is_rewinding(cx) {
            return;
        }
        let answering = !self.snapshot.pending.user_inputs.is_empty();
        // `/side` beats an open question; where side chats are unavailable it stays a literal answer.
        if self.editing_queued().is_none()
            && let Some(prompt) = t3_core::composer::menu::parse_side_chat_command(&self.prompt(cx))
            && !(answering && self.side_chat_disabled_reason(cx).is_some())
        {
            self.send_side_chat(prompt, window, cx);
            return;
        }
        if answering {
            self.advance(window, cx);
            return;
        }
        if self.editing_queued().is_some() {
            self.save_queued_edit(window, cx);
            return;
        }
        if let Some(target) = self.snapshot.target.clone() {
            self.sync_upload_states_for(&target, cx);
        }
        let prompt = self.prompt(cx);
        let staged = self.staged_attachments();
        if self.fresh_session_command_available(cx)
            && !self.has_non_prompt_content(&prompt, &staged)
            && rules::is_clear_composer_command(&prompt)
        {
            self.arm_fresh_session(cx);
            self.set_prompt("", window, cx);
            self.draft_changed(cx);
            return;
        }
        if t3_core::usage::limits::UsageLimitsReport::is_command(&prompt)
            && !self.has_non_prompt_content(&prompt, &staged)
            && self.usage_limits_offered(cx)
            && self.usage_limits_key(cx).is_some()
        {
            if self.open_usage_limits(cx) {
                self.set_prompt("", window, cx);
                self.draft_changed(cx);
            }
            return;
        }
        if self.snapshot.setup_blocks_send {
            return;
        }
        // Excerpts whose text is gone are left out, links and all (`snapshotComposerSend`).
        let terminals = TerminalSend::of(&prompt, self.current_terminals());
        if !terminals.has_content(
            staged.len(),
            self.linked_threads(&prompt)
                + t3_core::composer::review::referenced(&prompt, self.current_reviews()).len()
                + usize::from(self.current_reply_to().is_some()),
        ) {
            if terminals.expired > 0 {
                self.expired_toast(terminals.expired, ExpiredNotice::Empty, cx);
            } else if self.snapshot.can_resume && intent == SubmitIntent::Foreground {
                self.resume(window, cx);
            }
            return;
        }
        let text = terminals.text;
        if let Some(target) = self.attachment_target()
            && self.attachments.preparations.count(&super::drafts::key(&target)) > 0
        {
            self.error = Some("Wait for attachments to finish preparing before sending.".to_owned());
            cx.notify();
            return;
        }
        if self.staged_needs_reattach() {
            self.error = Some("Attach the interrupted file again or remove it".to_owned());
            cx.notify();
            return;
        }
        let file_refs: Vec<(&str, u64)> = staged
            .iter()
            .filter(|a| a.kind == t3_core::composer::attachments::AttachmentKind::File)
            .map(|a| (a.name.as_str(), a.size_bytes))
            .collect();
        if !file_refs.is_empty() {
            let config = self.state.read(cx).server_config.config.as_ref();
            let capability_known = config.is_some();
            let file_cap = config.and_then(|c| c.environment.capabilities.file_attachments);
            if let Some(reason) = attachments::image::file_attachment_capability_block_reason(
                capability_known,
                file_cap.is_some(),
                file_cap.map(|c| c.max_upload_bytes),
                &file_refs,
            ) {
                self.error = Some(reason);
                cx.notify();
                return;
            }
        }
        let block_reason = if self.snapshot.pending.approvals.is_empty() {
            attachments::send_block_reason(&staged)
        } else {
            attachments::approval_redirect_block_reason(&staged)
        };
        if let Some(reason) = block_reason {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        if staged.iter().any(|attachment| attachment.attachment_id().is_none()) {
            self.await_uploads(intent, window, cx);
            return;
        }
        let outgoing = self.outgoing(text.clone(), &staged);
        if let Some(validation_message) = rules::prompt_length_validation_message(&text, Some(&outgoing.text)) {
            self.prompt_length_error = Some(validation_message);
            cx.notify();
            return;
        }
        self.prompt_length_error = None;
        if terminals.expired > 0 {
            self.expired_toast(terminals.expired, ExpiredNotice::Omitted, cx);
        }
        match self.snapshot.target.clone() {
            Some(Target::Draft(project_id)) => self.launch(project_id, outgoing, intent, window, cx),
            Some(Target::Thread(thread_id)) => self.send(thread_id, outgoing, intent, window, cx),
            _ => {}
        }
    }

    /// `buildExpiredTerminalContextToastCopy` as a warning.
    fn expired_toast(&self, count: usize, notice: ExpiredNotice, cx: &mut Context<Self>) {
        let (title, description) = expired_toast_copy(count, notice);
        let toast = Toast::new(ToastKind::Warning, title).description(description);
        self.state.update(cx, |state, cx| state.show_toast(toast, cx));
    }

    /// `sideChatDisabledReason` for this composer; a side chat's own composer cannot start one.
    pub(super) fn side_chat_disabled_reason(&self, cx: &App) -> Option<&'static str> {
        if self.fixed.is_some() {
            return Some("Side chats cannot start side chats.");
        }
        self.state.read(cx).side_chat_disabled_reason()
    }

    /// `/side` and `/btw` as the whole message: its text starts a side chat of the open thread.
    fn send_side_chat(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        let warn = |title: &'static str, description: &'static str| Toast::new(ToastKind::Warning, title).description(description);
        if let Some(reason) = self.side_chat_disabled_reason(cx) {
            self.state.update(cx, |state, cx| state.show_toast(warn("Side chat not started", reason), cx));
            return;
        }
        let staged = self.staged_attachments();
        if !staged.is_empty() || self.has_context_records(&self.prompt(cx)) || self.current_reply_to().is_some() {
            let toast = warn("Side chats take text only", "Remove the attachments and context, then send it again.");
            self.state.update(cx, |state, cx| state.show_toast(toast, cx));
            return;
        }
        if self.state.update(cx, |state, cx| state.open_side_chat(Some(prompt), cx)) {
            self.set_prompt("", window, cx);
            self.draft_changed(cx);
        }
    }

    pub(super) fn question_revision(&self, cx: &App) -> Vec<(String, String)> {
        let Some(Target::Thread(id)) = self.snapshot.target.as_ref() else { return Vec::new() };
        self.state.read(cx).thread_state(id).projection.as_ref().map(attachments::question_revision).unwrap_or_default()
    }

    pub(super) fn cancel_pending_send(&mut self) -> bool {
        let cancelled = self.pending_send.cancel();
        if cancelled { self.sending = false; }
        cancelled
    }

    fn await_uploads(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let connection = self.state.read(cx).connection_id();
        let transport = self.state.read(cx).client_handle().map(|client| *client.connection_generation().borrow());
        self.send_connection = Some((connection, transport, self.state.read(cx).phase.clone()));
        let retry_ids = attachments::retry_on_send(&self.staged_attachments());
        let wait_task = self.await_target_uploads(&target, &retry_ids, cx);
        self.pending_questions = self.question_revision(cx);
        let generation = self.pending_send.begin();
        self.sending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(wait_task) = wait_task {
                wait_task.await;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.pending_send.finish(generation) { return; }
                this.sending = false;
                if this.is_rewinding(cx) {
                    cx.notify();
                    return;
                }
                this.sync_upload_states_for(&target, cx);
                let same_connection = matches!(this.state.read(cx).phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected))
                    && this.question_revision(cx) == this.pending_questions
                    && this.state.read(cx).connection_id() == connection
                    && this.state.read(cx).client_handle().map(|client| *client.connection_generation().borrow()) == transport;
                if same_connection {
                    if this.staged_needs_reattach() {
                        this.error = Some("Attach the interrupted file again or remove it".into());
                    } else {
                        match attachments::uploaded_for_send(&this.staged_attachments()) {
                            Ok(_) => this.submit(intent, window, cx),
                            Err(reason) => this.error = Some(reason.into()),
                        }
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn outgoing(&self, prompt: String, staged: &[Attachment]) -> Outgoing {
        let prompt_for_send = match self.current_reply_to() {
            Some(reply) => {
                let cid = t3_core::composer::records::field(reply, "contextId");
                if !t3_core::composer::menu::referenced_context_ids(&prompt).contains(&cid)
                    && let Some(reference) = t3_core::composer::records::reference(reply)
                {
                    format!("{reference} {prompt}")
                } else {
                    prompt.clone()
                }
            }
            None => prompt.clone(),
        };
        let text = attachments::text_for_send(&prompt_for_send, !staged.is_empty());
        Outgoing {
            title: rules::thread_title_seed(&prompt_for_send, staged),
            context: self.message_context(&prompt_for_send, staged),
            attachments: attachments::chat_attachments(staged).unwrap_or_default(),
            text,
            prompt,
        }
    }

    /// The model selection a turn carries, with the options `getComposerProviderState` dispatches.
    fn dispatch_selection(resolved: &Resolved) -> Option<ModelSelection> {
        let model = resolved.model.as_ref()?;
        Some(traits::model_selection(&model.instance_id, &model.model, &resolved.dispatch_options))
    }

    fn launch(&mut self, project_id: String, outgoing: Outgoing, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        if !resolved.multi_models.is_empty() {
            self.launch_multiple(project_id, resolved, outgoing, window, cx);
            return;
        }
        let Some(selection) = Self::dispatch_selection(&resolved) else {
            self.error = Some(NO_PROVIDER.into());
            cx.notify();
            return;
        };
        let state = self.state.read(cx);
        let star_origin = state.star_prompt_origin();
        let config = state.server_config.config.as_ref();
        let root = state.shell.project(&project_id).map(|project| project.workspace_root.clone());
        let configured = config.and_then(|config| workspace::configured_env_mode(&config.settings, &project_id));
        let workspace_choice = self.selected_workspace();
        let target = Target::Draft(project_id.clone());
        let stored_workspace = self.drafts.store.get(&super::drafts::key(&target)).and_then(|draft| draft.workspace.clone());
        let destination = (
            state.environments.active.clone(),
            config.map(|config| config.environment.environment_id.clone()),
            state.shell.project(&project_id).map(|project| project.created_at.clone()),
        );
        if stored_workspace.as_ref().is_some_and(|choice| {
            workspace_choice.as_ref() != Some(choice)
                || !choice.in_scope(destination.0.as_deref().unwrap_or_default(), root.as_deref().unwrap_or_default())
                || choice.destination.as_ref().is_some_and(|(environment, created)| destination.1.as_ref() != Some(environment) || destination.2.as_ref() != Some(created))
        }) {
            self.error = Some("The destination changed before launch. Select the workspace again.".into());
            cx.notify();
            return;
        }
        let origin = (state.connection_id(), state.client_handle().map(|client| *client.connection_generation().borrow()), state.environment().map(str::to_owned));
        let new_projects_root = config.and_then(|config| config.new_projects_root.clone());
        // Settled threads the shell left out still count, as `readUnloadedThreadCount` does.
        let has_threads = state.shell.snapshot.as_ref().is_some_and(|shell| {
            shell.threads.iter().any(|t| t.project_id == project_id)
                || shell.older_settled.as_ref().is_some_and(|cut| cut.counts.iter().any(|c| c.project_id == project_id && c.count > 0))
        });
        let start_from_origin = config.is_some_and(|config| workspace::start_from_origin(&config.settings, &project_id, ThreadEnvMode::Worktree));
        let cwd = root.clone().unwrap_or_default();
        let project_file = state.rpc(method::PROJECTS_READ_FILE, workspace::project_file_request(&cwd));
        let refs = workspace_choice
            .as_ref()
            .and_then(workspace::DraftWorkspace::strategy)
            .is_none()
            .then(|| state.request(method::VCS_LIST_REFS, workspace::list_refs_request(&cwd), |value| serde_json::from_value::<VcsRefs>(value).map_err(|error| error.to_string())))
            .flatten();
        self.record_draft(&target, cx);
        let taken = self.begin_send(&target, &outgoing.prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let file_mode = match (&configured, &root) {
                (None, Some(_)) if workspace_choice.is_none() => project_file
                    .await
                    .ok()
                    .and_then(|result| workspace::project_file_contents(&result).and_then(workspace::project_file_env_mode)),
                _ => None,
            };
            let mode = workspace_choice.as_ref().map_or_else(|| workspace::new_thread_env_mode(configured, file_mode, root.as_deref(), new_projects_root.as_deref(), has_threads), |choice| choice.mode);
            let selected = workspace_choice.as_ref().and_then(workspace::DraftWorkspace::strategy);
            let refs = match (mode, &root) {
                (ThreadEnvMode::Worktree, Some(_)) if selected.is_none() => match refs {Some(task) => task.await.ok().and_then(Result::ok), None => None},
                _ => None,
            };
            let strategy = workspace::effective_strategy(workspace_choice.as_ref(), mode, refs.as_ref(), start_from_origin);
            let launch = this.update_in(cx, |this, _, cx| {
                let state = this.state.read(cx);
                let current = (state.connection_id(), state.client_handle().map(|client| *client.connection_generation().borrow()), state.environment().map(str::to_owned));
                let current_destination = (
                    state.environments.active.clone(),
                    state.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()),
                    state.shell.project(&project_id).map(|project| project.created_at.clone()),
                );
                let current_workspace = this.drafts.store.get(&super::drafts::key(&target)).and_then(|draft| draft.workspace.as_ref());
                if current_destination != destination || current_workspace != stored_workspace.as_ref() || current != origin || state.selected_thread.is_some() || state.draft_project.as_deref() != Some(project_id.as_str()) || this.snapshot.target.as_ref() != Some(&target) || state.shell.project(&project_id).map(|project| &project.workspace_root) != root.as_ref() {
                    return Err("The destination changed before launch. Select the workspace again.".to_owned());
                }
                let strategy = strategy.map_err(str::to_owned)?;
                let (thread_id, command_id, message_id) = (uuid_v4(), uuid_v4(), uuid_v4());
                let payload = commands::launch_thread(&LaunchThread {
                    command_id: &command_id,
                    thread_id: &thread_id,
                    project_id: &project_id,
                    title: &outgoing.title,
                    model_selection: &selection,
                    runtime_mode: resolved.runtime_mode,
                    interaction_mode: resolved.interaction_mode,
                    workspace: &strategy,
                    message_id: &message_id,
                    text: &outgoing.text,
                    context: outgoing.context.as_ref(),
                    attachments: &outgoing.attachments,
                });
                // "Work locally" on the setup card resends this launch in the project checkout.
                let connection = this.state.read(cx).connection_id();
                let worktree_launch = connection.filter(|_| matches!(strategy, workspace::Strategy::Worktree { .. })).map(|connection| {
                    t3_core::worktree_setup::DraftLaunch { thread_id: thread_id.clone(), connection, launch: payload.clone() }
                });
                this.state.update(cx, |state, cx| state.mark_launching_thread(&thread_id, cx));
                Ok::<_, String>((thread_id, worktree_launch, this.state.read(cx).rpc(method::LAUNCH_THREAD, payload)))
            });
            let executor = cx.background_executor().clone();
            let (result, failed_thread_id, worktree_launch) = match launch {
                Ok(Ok((thread_id, worktree_launch, request))) => match with_dispatch_timeout(&executor, request).await {
                    Ok(Ok(_)) => (Ok(thread_id), None, worktree_launch),
                    Ok(Err(error)) => (Err(error.to_string()), Some(thread_id), worktree_launch),
                    Err(timeout) => (Err(timeout.to_owned()), Some(thread_id), worktree_launch),
                },
                Ok(Err(error)) => (Err(error), None, None),
                Err(_) => return,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(failed_id) = failed_thread_id {
                    this.state.update(cx, |state, cx| state.clear_launching_thread(&failed_id, cx));
                }
                if result.is_ok() {
                    this.clear_usage_limits_for(&taken.target);
                    this.state.update(cx, |state, cx| state.record_prompt_sent(star_origin, cx));
                }
                this.finish_launch(intent, (result, worktree_launch), taken, window, cx);
            });
        })
        .detach();
    }

    fn launch_multiple(&mut self, project_id: String, resolved: Resolved, outgoing: Outgoing, window: &mut Window, cx: &mut Context<Self>) {
        let (supports_bootstrap, root, start_from_origin, star_origin, origin, destination, dispatch_selections) = {
            let state = self.state.read(cx);
            let config = state.server_config.config.as_ref();
            let supports_bootstrap = config.is_some_and(|c| c.environment.capabilities.required_worktree_bootstrap == Some(true));
            let root = state.shell.project(&project_id).map(|project| project.workspace_root.clone());
            let start_from_origin = config.is_some_and(|c| workspace::start_from_origin(&c.settings, &project_id, ThreadEnvMode::Worktree));
            let star_origin = state.star_prompt_origin();
            let origin = (
                state.connection_id(),
                state.client_handle().map(|client| *client.connection_generation().borrow()),
                state.environment().map(str::to_owned),
            );
            let destination = (
                state.environments.active.clone(),
                config.map(|config| config.environment.environment_id.clone()),
                state.shell.project(&project_id).map(|project| project.created_at.clone()),
            );
            let cwd = self.menu_cwd(cx);
            let dispatch_selections: Vec<(ModelSelection, ModelSelection)> = resolved
                .multi_models
                .iter()
                .map(|raw| {
                    let dispatch = config
                        .and_then(|config| config.provider(&raw.instance_id))
                        .map(|provider| {
                            let snapshot = self.workspace_snapshot_for(provider, cwd.as_deref());
                            let provider_model =
                                t3_core::providers::models_for_cwd(provider, snapshot).iter().find(|m| m.slug == raw.model);
                            let explicit = traits::selections(raw.options.as_ref());
                            let options = traits::composer_options(&provider.driver, provider_model, false, &explicit);
                            traits::model_selection(&raw.instance_id, &raw.model, &options.dispatch)
                        })
                        .unwrap_or_else(|| raw.clone());
                    (raw.clone(), dispatch)
                })
                .collect();
            (supports_bootstrap, root, start_from_origin, star_origin, origin, destination, dispatch_selections)
        };
        if !supports_bootstrap {
            self.error = Some("Update this server before starting multiple models.".into());
            cx.notify();
            return;
        }
        let Some(cwd) = root.filter(|_| self.workspace_is_repo()) else {
            let toast = Toast::new(ToastKind::Warning, "Choose models and a base branch")
                .description("Multiple models need a new thread in a Git project. Each gets its own worktree.");
            self.state.update(cx, |state, cx| state.show_toast(toast, cx));
            return;
        };
        let target = Target::Draft(project_id.clone());
        let refs = self.state.read(cx).request(
            method::VCS_LIST_REFS,
            workspace::list_refs_request(&cwd),
            |value| serde_json::from_value::<VcsRefs>(value).map_err(|error| error.to_string()),
        );
        self.record_draft(&target, cx);
        let taken = self.begin_send(&target, &outgoing.prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let refs = match refs {
                Some(task) => task.await.ok().and_then(Result::ok),
                None => None,
            };
            let strategy = workspace::launch_strategy(ThreadEnvMode::Worktree, refs.as_ref(), start_from_origin).map_err(str::to_owned);
            let requests = this.update_in(cx, |this, _, cx| {
                let state = this.state.read(cx);
                let current = (
                    state.connection_id(),
                    state.client_handle().map(|client| *client.connection_generation().borrow()),
                    state.environment().map(str::to_owned),
                );
                let current_destination = (
                    state.environments.active.clone(),
                    state.server_config.config.as_ref().map(|config| config.environment.environment_id.clone()),
                    state.shell.project(&project_id).map(|project| project.created_at.clone()),
                );
                if current_destination != destination
                    || current != origin
                    || state.selected_thread.is_some()
                    || state.draft_project.as_deref() != Some(project_id.as_str())
                    || this.snapshot.target.as_ref() != Some(&target)
                {
                    return Err("The destination changed before launch. Select the workspace again.".to_owned());
                }
                let strategy = strategy?;
                let rpcs = dispatch_selections
                    .iter()
                    .map(|(raw, dispatch)| {
                        let (thread_id, command_id, message_id) = (uuid_v4(), uuid_v4(), uuid_v4());
                        let payload = commands::launch_thread(&LaunchThread {
                            command_id: &command_id,
                            thread_id: &thread_id,
                            project_id: &project_id,
                            title: &outgoing.title,
                            model_selection: dispatch,
                            runtime_mode: resolved.runtime_mode,
                            interaction_mode: resolved.interaction_mode,
                            workspace: &strategy,
                            message_id: &message_id,
                            text: &outgoing.text,
                            context: outgoing.context.as_ref(),
                            attachments: &outgoing.attachments,
                        });
                        ((raw.clone(), thread_id), payload)
                    })
                    .collect::<Vec<_>>();
                this.state.update(cx, |state, cx| {
                    for ((_, thread_id), _) in &rpcs {
                        state.mark_launching_thread(thread_id, cx);
                    }
                });
                let state = this.state.read(cx);
                Ok::<_, String>(rpcs.into_iter().map(|(meta, payload)| (meta, state.rpc(method::LAUNCH_THREAD, payload))).collect::<Vec<_>>())
            });
            let executor = cx.background_executor().clone();
            let (started_count, failed_selections, failed_thread_ids, failure_msg) = match requests {
                Ok(Ok(rpcs)) => {
                    let (metas, futs): (Vec<_>, Vec<_>) = rpcs.into_iter().unzip();
                    match with_dispatch_timeout(&executor, futures::future::join_all(futs)).await {
                        Ok(outcomes) => {
                            let mut started = 0usize;
                            let mut failed = Vec::new();
                            let mut failed_ids = Vec::new();
                            let mut first_error = None;
                            for ((raw, tid), outcome) in metas.into_iter().zip(outcomes) {
                                match outcome {
                                    Ok(_) => started += 1,
                                    Err(err) => {
                                        if first_error.is_none() {
                                            first_error = Some(err.to_string());
                                        }
                                        failed.push(raw);
                                        failed_ids.push(tid);
                                    }
                                }
                            }
                            (started, failed, failed_ids, first_error)
                        }
                        Err(timeout) => {
                            let (raws, tids): (Vec<_>, Vec<_>) = metas.into_iter().unzip();
                            (0, raws, tids, Some(timeout.to_owned()))
                        }
                    }
                }
                Ok(Err(error)) => {
                    let failed = dispatch_selections.into_iter().map(|(raw, _)| raw).collect();
                    (0, failed, Vec::new(), Some(error))
                }
                Err(_) => return,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                if !failed_thread_ids.is_empty() {
                    this.state.update(cx, |state, cx| {
                        for tid in &failed_thread_ids {
                            state.clear_launching_thread(tid, cx);
                        }
                    });
                }
                if started_count > 0 {
                    this.retire_sent_attachments(&taken.target, &taken.attachments, cx);
                    this.clear_usage_limits_for(&taken.target);
                    this.state.update(cx, |state, cx| {
                        state.record_prompt_sent(star_origin, cx);
                        let noun = if started_count == 1 { "thread" } else { "threads" };
                        state.show_toast(Toast::new(ToastKind::Success, format!("Started {started_count} {noun} in background")), cx);
                    });
                }
                if failed_selections.is_empty() {
                    if !this.current_draft(&taken.target, cx).has_user_content() {
                        this.discard_draft(&taken.target, cx);
                    }
                    if let Some(choices) = this.choices.get_mut(&taken.target) {
                        choices.multi_models.clear();
                    }
                } else {
                    if let Some(choices) = this.choices.get_mut(&taken.target) {
                        if failed_selections.len() > 1 {
                            choices.multi_models = failed_selections;
                        } else {
                            choices.multi_models.clear();
                            choices.model = failed_selections.into_iter().next();
                        }
                    }
                    this.restore(taken, failure_msg.unwrap_or_else(|| "Failed to send messages.".to_owned()), window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_launch(&mut self, intent: SubmitIntent, (result, worktree_launch): (Result<String, String>, Option<t3_core::worktree_setup::DraftLaunch>), taken: Taken, window: &mut Window, cx: &mut Context<Self>) {
        self.sending = false;
        let shown = self.state.read(cx).environment() == taken.environment.as_deref() && self.snapshot.target.as_ref() == Some(&taken.target);
        match result {
            Err(error) => self.restore(taken, error, window, cx),
            Ok(thread_id) => {
                self.retire_sent_attachments(&taken.target, &taken.attachments, cx);
                if intent != SubmitIntent::Background && !self.current_draft(&taken.target, cx).has_user_content() { self.discard_draft(&taken.target, cx); }
                if shown && intent != SubmitIntent::Background {
                    if let (Some(launch), Some(setup)) = (worktree_launch, self.setup.clone()) {
                        setup.update(cx, |setup, cx| setup.record_launch(launch, cx));
                    }
                    self.state.update(cx, |state, cx| state.select_when_listed(&thread_id, cx));
                }
            }
        }
        cx.notify();
    }

    /// `useComposerResume`: resumes an interrupted or usage-limited run (`manualContinuationOfRunId`)
    /// and/or releases held queued runs (`queue.resume`).
    pub(super) fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending || self.is_rewinding(cx) || !self.snapshot.can_resume || !self.snapshot.can_operate_thread {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        let resolved = self.resolve(cx);
        let selection = Self::dispatch_selection(&resolved);
        let mut steps: Vec<(Value, &'static str)> = Vec::new();
        if let Some(resumable_run_id) = self.snapshot.resumable_run_id.as_deref() {
            if Some(resolved.runtime_mode) != self.snapshot.thread_runtime {
                steps.push((commands::set_runtime_mode(&uuid_v4(), &thread_id, resolved.runtime_mode), "Could not resume thread."));
            }
            if Some(resolved.interaction_mode) != self.snapshot.thread_interaction {
                steps.push((commands::set_interaction_mode(&uuid_v4(), &thread_id, resolved.interaction_mode), "Could not resume thread."));
            }
            steps.push((
                commands::resume_thread_turn(&uuid_v4(), &thread_id, &uuid_v4(), resumable_run_id, selection.as_ref()),
                "Could not resume thread.",
            ));
            if self.snapshot.has_held_queued_runs {
                steps.push((commands::resume_thread_queue(&uuid_v4(), &thread_id), "Could not resume queued runs."));
            }
        } else {
            steps.push((commands::resume_thread_queue(&uuid_v4(), &thread_id), "Could not resume thread."));
        }
        let target = Target::Thread(thread_id);
        let failures: Vec<&'static str> = steps.iter().map(|(_, failure)| *failure).collect();
        let request = self.state.read(cx).dispatch_in_order(steps.into_iter().map(|(command, _)| command).collect());
        self.sending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let failure = match with_dispatch_timeout(&executor, request).await {
                Ok(Ok(())) => None,
                Ok(Err((index, error))) => {
                    Some(format!("{} {error}", failures.get(index).copied().unwrap_or("Could not resume thread.")))
                }
                Err(timeout) => Some(timeout.to_owned()),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.sending = false;
                if let Some(error) = failure {
                    this.error = Some(error);
                } else {
                    this.clear_usage_limits_for(&target);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, thread_id: String, outgoing: Outgoing, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        let resolved = self.resolve(cx);
        let Some(selection) = Self::dispatch_selection(&resolved) else {
            self.error = Some(NO_PROVIDER.into());
            cx.notify();
            return;
        };
        let settings = self.state.read(cx).client_settings;
        let approvals = self.snapshot.pending.approvals.clone();
        let fresh_arm = self.fresh_session_arm(cx);
        let fresh_context = fresh_arm.is_some();
        let follow_up = if approvals.is_empty() { settings.follow_up_behavior } else { FollowUpBehavior::Steer };
        let can_steer = !self.snapshot.compaction_blocks_steer && !fresh_context;
        let base_dispatch = rules::dispatch_mode(self.snapshot.running, intent == SubmitIntent::Alternate, follow_up, can_steer);
        let compact_offered = self.compact_before_send_tokens(&resolved, cx).is_some();
        let keep_full_history = self.is_keep_full_history(cx);
        let plan = rules::plan_composer_turn(
            &outgoing.text,
            self.snapshot.setup_queues,
            compact_offered,
            keep_full_history,
            base_dispatch,
            fresh_context,
        );
        let feedback = approvals.first().filter(|approval| approval.supports_feedback && approval.live).map(|approval| approval.request_id.clone());
        if feedback.is_some() && (!outgoing.attachments.is_empty() || outgoing.context.is_some()) {
            self.error = Some("Send approval feedback without attachments or added context.".to_owned());
            cx.notify();
            return;
        }
        let mut steps: Vec<(Value, &'static str)> = Vec::new();
        if let Some(request_id) = &feedback {
            let command = commands::respond_to_approval(&uuid_v4(), &thread_id, request_id, ApprovalDecision::Decline, Some(&outgoing.text));
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
            if plan.compact_before_send {
                let (compact_cmd, compact_msg) = (uuid_v4(), uuid_v4());
                steps.push((
                    commands::composer_message(&ComposerMessage {
                        command_id: &compact_cmd,
                        thread_id: &thread_id,
                        message_id: &compact_msg,
                        text: "/compact",
                        title_seed: &outgoing.title,
                        model_selection: &selection,
                        dispatch: commands::ComposerDispatch::Auto,
                        context: None,
                        attachments: &[],
                        fresh_context: false,
                        idle_compaction: Some("onReturn"),
                    }),
                    "Failed to compact thread before sending.",
                ));
            }
            let (command_id, message_id) = (uuid_v4(), uuid_v4());
            let message = ComposerMessage {
                command_id: &command_id,
                thread_id: &thread_id,
                message_id: &message_id,
                text: &outgoing.text,
                title_seed: &outgoing.title,
                model_selection: &selection,
                dispatch: plan.turn_dispatch,
                context: outgoing.context.as_ref(),
                attachments: &outgoing.attachments,
                fresh_context,
                idle_compaction: None,
            };
            steps.push((commands::composer_message(&message), "Failed to send message."));
        }
        let sends_prompt = t3_core::github_star_prompt::plan_sends_prompt(steps.iter().map(|(command, _)| command));
        let failures: Vec<&'static str> = steps.iter().map(|(_, failure)| *failure).collect();
        let star_origin = self.state.read(cx).star_prompt_origin();
        let request = self.state.read(cx).dispatch_in_order(steps.into_iter().map(|(command, _)| command).collect());
        let target = Target::Thread(thread_id);
        let taken = self.begin_send(&target, &outgoing.prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let failure = match with_dispatch_timeout(&executor, request).await {
                Ok(Ok(())) => None,
                Ok(Err((index, error))) => {
                    Some(format!("{} {error}", failures.get(index).copied().unwrap_or("Failed to send message.")))
                }
                Err(timeout) => Some(timeout.to_owned()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                if let Some(error) = failure {
                    this.restore(taken, error, window, cx);
                } else {
                    if let Some(arm) = fresh_arm {
                        this.consume_fresh_session_arm(arm, cx);
                    }
                    this.clear_keep_full_history(taken.environment.as_deref(), &taken.target);
                    this.retire_sent_attachments(&taken.target, &taken.attachments, cx);
                    this.clear_usage_limits_for(&taken.target);
                    if sends_prompt {
                        this.state.update(cx, |state, cx| state.record_prompt_sent(star_origin, cx));
                    }
                    let shown = this.state.read(cx).environment() == taken.environment.as_deref()
                        && this.snapshot.target.as_ref() == Some(&taken.target);
                    if intent == SubmitIntent::Background
                        && shown
                        && let Target::Thread(ref thread_id) = taken.target
                    {
                        cx.emit(SendAndNewThreadCompleted { thread_id: thread_id.clone() });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Clears the prompt and takes the attachments and the context `prompt` links, as the web
    /// clears the composer on send. Whatever is added while the send is in flight stays.
    fn begin_send(&mut self, target: &Target, prompt: &str, window: &mut Window, cx: &mut Context<Self>) -> Taken {
        let draft = self.current_draft(target, cx);
        let environment = self.state.read(cx).environment().map(str::to_owned);
        self.sending = true;
        self.error = None;
        self.set_prompt("", window, cx);
        cx.notify();
        let taken = Taken {
            target: target.clone(), environment, draft,
            attachments: self.take_attachments(target),
            threads: self.take_threads(target, prompt),
            records: self.take_records(target, prompt),
            terminals: self.take_terminals(target, prompt),
            reviews: self.take_reviews(target, prompt),
            reply_to: self.take_reply_to(target),
        };
        self.record_draft(target, cx);
        taken
    }

    /// `useComposerSend` restores the captured target, independently of the current route.
    fn restore(&mut self, taken: Taken, error: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = &taken.target;
        let shown = self.state.read(cx).environment() == taken.environment.as_deref() && self.snapshot.target.as_ref() == Some(target);
        let live = self.current_draft(target, cx);
        let text = taken.draft.restored_prompt();
        if self.mutate_drafts(cx, |store| store.restore(&super::drafts::key(target), taken.draft, &live)) {
            self.restore_attachments(target, taken.attachments);
            self.restore_threads(target, taken.threads);
            self.seed_records(target, taken.records);
            self.restore_terminals(target, taken.terminals);
            self.seed_reviews(target, taken.reviews);
            self.seed_reply_to(target, taken.reply_to);
            self.schedule_draft_save(target, cx);
            if shown { self.set_prompt(&text, window, cx); }
        }
        if shown {self.error = Some(error);} else {tracing::warn!(%error, "Send failed; retained the original target's draft");}
    }
}

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn verify_failed_launch_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        use t3_core::composer::attachments::{AttachmentKind, Upload};
        use t3_core::composer::menu::ThreadContext;
        let original = self.snapshot.target.clone().ok_or("open a draft for recovery fixture")?;
        let other = Target::Draft("fixture-other-project".into());
        for (error, concurrent_edit) in [("destination changed", false), ("RPC failure", false), ("RPC failure", true)] {
            let mention = ThreadContext::new("fixture-env", "fixture-thread", "Original context");
            let text = format!("recover original {}", mention.reference());
            self.set_prompt(&text, window, cx);
            self.seed_mentions(&original, vec![mention]);
            self.seed_attachments(&original, vec![Attachment {id: "fixture-attachment".into(), kind: AttachmentKind::File, name: "notes.md".into(), mime_type: "text/markdown".into(), size_bytes: 12, pasted_text: false, upload: Upload::Ready {attachment_id: "fixture-upload".into()}}], false, cx);
            self.record_draft(&original, cx);
            let sent = self.drafts.store.get(&super::drafts::key(&original)).unwrap().clone();
            let taken = self.begin_send(&original, &text, window, cx);
            self.switch_draft(Some(&original), Some(&other), window, cx);
            self.snapshot.target = Some(other.clone());
            self.set_prompt("other draft edits", window, cx);
            let mut expected = sent.clone();
            if concurrent_edit {
                expected.prompt = "concurrent edit in original target".into();
                expected.attachments[0].id = "concurrent-attachment".into();
                self.mutate_drafts(cx, |store| store.set(&super::drafts::key(&original), expected.clone()));
            }
            self.finish_launch(SubmitIntent::Foreground, (Err(error.into()), None), taken, window, cx);
            let recovered = self.drafts.store.get(&super::drafts::key(&original)).ok_or("failed send lost original draft")?;
            if recovered.prompt != expected.prompt || recovered.attachments != expected.attachments || recovered.threads != expected.threads { return Err("failed send lost original prompt/attachments/context".into()); }
            let persisted = t3_core::composer::drafts::DraftStore::from_value(Some(&self.drafts.store.to_value()));
            if persisted.get(&super::drafts::key(&original)) != Some(recovered) {return Err("recovery did not survive draft persistence".into());}
            if !concurrent_edit && (self.staged_for(&original).len() != 1 || self.mentions(&original).is_empty()) {return Err("failed send did not restore original resource ownership".into());}
            if self.prompt(cx) != "other draft edits" {return Err("recovery overwrote selected draft".into());}
            self.switch_draft(Some(&other), Some(&original), window, cx);
            self.snapshot.target = Some(original.clone());
            self.take_attachments(&original);
        }
        eprintln!("failed launch recovery: destination change, RPC failure, concurrent edits and persisted original attachments/context passed; no launch RPC");
        Ok(())
    }
}

#[cfg(all(test, feature = "snapshot"))]
impl Composer {
    pub(crate) fn exercise_menu_launch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Draft(project)) = self.snapshot.target.clone() else { panic!("fixture needs a draft") };
        let outgoing = self.outgoing(self.prompt(cx), &[]);
        self.launch(project, outgoing, SubmitIntent::Foreground, window, cx);
    }
    pub(crate) fn menu_launch_outcome(&self) -> (bool, Option<String>) { (self.sending, self.error.clone()) }
}
