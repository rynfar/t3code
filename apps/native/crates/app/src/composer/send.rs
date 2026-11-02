//! What Send does (`useComposerSend.ts`): launch a draft as a new thread (`startThreadTurn` with
//! `bootstrap.createThread`, in a worktree when the project's default asks for one), or send to a
//! thread after declining what it waits on and persisting permissions and mode, each step after
//! the previous one committed, as `declinePendingApprovals` and `persistThreadSettingsForNextTurn`
//! await them.

use gpui::{App, Context, Window};
use serde_json::Value;
use t3_core::commands::{self, ApprovalDecision, ComposerMessage, LaunchThread, method};
use t3_core::composer::attachments::{self, Attachment};
use t3_core::composer::context::{ExpiredNotice, TerminalContext, TerminalSend, expired_toast_copy};
use t3_core::composer::menu::{SlashCommand, ThreadContext};
use t3_core::composer::{self as rules, FollowUpBehavior, SubmitIntent};
use t3_core::model::ModelSelection;
use t3_core::server_config::{InteractionMode, ThreadEnvMode};
use t3_core::traits;
use t3_core::workspace::{self, VcsRefs};

use super::{Composer, Resolved, Target};
use crate::app_state::uuid_v4;
use crate::toast::{Toast, ToastKind};

const NO_PROVIDER: &str = "Enable a provider in Settings to send a message";

/// What a send took from the composer, put back if it fails.
pub(super) struct Taken {
    attachments: Vec<super::attachments::Staged>,
    threads: Vec<ThreadContext>,
    terminals: Vec<TerminalContext>,
}

/// What one send carries, resolved before the prompt clears.
struct Outgoing {
    /// The prompt as typed, put back if the send fails.
    prompt: String,
    /// What the message says: the prompt, or the attachment-only prompt.
    text: String,
    title: String,
    context: Option<Value>,
    attachments: Vec<Value>,
}

impl Composer {
    /// Submit, as the send button and Enter do.
    pub(super) fn submit(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
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
        if self.snapshot.setup_blocks_send {
            return;
        }
        let prompt = self.prompt(cx);
        let staged = self.staged_attachments();
        // Excerpts whose text is gone are left out, links and all (`snapshotComposerSend`).
        let terminals = TerminalSend::of(&prompt, self.current_terminals());
        if !terminals.has_content(staged.len(), self.linked_threads(&prompt)) {
            if terminals.expired > 0 {
                self.expired_toast(terminals.expired, ExpiredNotice::Empty, cx);
            }
            return;
        }
        let text = terminals.text;
        if let Some(reason) = attachments::send_block_reason(&staged) {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        // `/plan` or `/default` alone switches the mode instead of sending.
        if let Some(command) = t3_core::composer::menu::parse_standalone_slash_command(&text)
            && staged.is_empty()
            && self.resolve(cx).plan_enabled
        {
            let mode = if command == SlashCommand::Plan { InteractionMode::Plan } else { InteractionMode::Default };
            if let Some(choices) = self.choices_mut() {
                choices.interaction_mode = Some(mode);
            }
            self.set_prompt("", window, cx);
            self.draft_changed(cx);
            cx.notify();
            return;
        }
        let outgoing = self.outgoing(text, &staged);
        if terminals.expired > 0 {
            self.expired_toast(terminals.expired, ExpiredNotice::Omitted, cx);
        }
        match self.snapshot.target.clone() {
            Some(Target::Draft(project_id)) => self.launch(project_id, outgoing, intent, window, cx),
            Some(Target::Thread(thread_id)) => self.send(thread_id, outgoing, intent, window, cx),
            None => {}
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
        if !staged.is_empty() || self.message_context(&self.prompt(cx), &staged).is_some() {
            let toast = warn("Side chats take text only", "Remove the attachments and context, then send it again.");
            self.state.update(cx, |state, cx| state.show_toast(toast, cx));
            return;
        }
        if self.state.update(cx, |state, cx| state.open_side_chat(Some(prompt), cx)) {
            self.set_prompt("", window, cx);
            self.draft_changed(cx);
        }
    }

    fn outgoing(&self, prompt: String, staged: &[Attachment]) -> Outgoing {
        let text = attachments::text_for_send(&prompt, !staged.is_empty());
        Outgoing {
            title: rules::thread_title_seed(&prompt, staged),
            context: self.message_context(&prompt, staged),
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
        let Some(selection) = Self::dispatch_selection(&resolved) else {
            self.error = Some(NO_PROVIDER.into());
            cx.notify();
            return;
        };
        let state = self.state.read(cx);
        let config = state.server_config.config.as_ref();
        let root = state.shell.project(&project_id).map(|project| project.workspace_root.clone());
        let configured = config.and_then(|config| workspace::configured_env_mode(&config.settings, &project_id));
        let new_projects_root = config.and_then(|config| config.new_projects_root.clone());
        // Settled threads the shell left out still count, as `readUnloadedThreadCount` does.
        let has_threads = state.shell.snapshot.as_ref().is_some_and(|shell| {
            shell.threads.iter().any(|t| t.project_id == project_id)
                || shell.older_settled.as_ref().is_some_and(|cut| cut.counts.iter().any(|c| c.project_id == project_id && c.count > 0))
        });
        let start_from_origin = config.is_some_and(|config| workspace::start_from_origin(&config.settings, &project_id, ThreadEnvMode::Worktree));
        let cwd = root.clone().unwrap_or_default();
        let project_file = state.rpc(method::PROJECTS_READ_FILE, workspace::project_file_request(&cwd));
        let refs = state.rpc(method::VCS_LIST_REFS, workspace::list_refs_request(&cwd));
        let target = Target::Draft(project_id.clone());
        let taken = self.begin_send(&target, &outgoing.prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let file_mode = match (&configured, &root) {
                (None, Some(_)) => project_file
                    .await
                    .ok()
                    .and_then(|result| workspace::project_file_contents(&result).and_then(workspace::project_file_env_mode)),
                _ => None,
            };
            let mode = workspace::new_thread_env_mode(configured, file_mode, root.as_deref(), new_projects_root.as_deref(), has_threads);
            let refs = match (mode, &root) {
                (ThreadEnvMode::Worktree, Some(_)) => refs.await.ok().and_then(|value| serde_json::from_value::<VcsRefs>(value).ok()),
                _ => None,
            };
            let strategy = workspace::launch_strategy(mode, refs.as_ref(), start_from_origin);
            let launch = this.update_in(cx, |this, _, cx| {
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
                let worktree_launch = matches!(strategy, workspace::Strategy::Worktree { .. }).then(|| payload.clone());
                Ok::<_, String>((thread_id, worktree_launch, this.state.read(cx).rpc(method::LAUNCH_THREAD, payload)))
            });
            let (result, worktree_launch) = match launch {
                Ok(Ok((thread_id, worktree_launch, request))) => (request.await.map(|_| thread_id).map_err(|error| error.to_string()), worktree_launch),
                Ok(Err(error)) => (Err(error), None),
                Err(_) => return,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                // A background launch leaves the draft open for the next thread.
                if result.is_ok() && intent != SubmitIntent::Background {
                    this.discard_draft(&target, cx);
                }
                match result {
                    Ok(_) if intent == SubmitIntent::Background => {}
                    Ok(thread_id) => {
                        if let (Some(launch), Some(setup)) = (worktree_launch, this.setup.clone()) {
                            setup.update(cx, |setup, cx| setup.record_launch(thread_id.clone(), launch, cx));
                        }
                        this.state.update(cx, |state, cx| state.select_when_listed(&thread_id, cx));
                    }
                    Err(error) => this.restore(&target, &outgoing.prompt, taken, error, window, cx),
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
        let follow_up = if approvals.is_empty() { settings.follow_up_behavior } else { FollowUpBehavior::Steer };
        let can_steer = !self.snapshot.compacting;
        let dispatch = rules::dispatch_mode(self.snapshot.running, intent == SubmitIntent::Alternate, follow_up, can_steer);
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
            let (command_id, message_id) = (uuid_v4(), uuid_v4());
            let message = ComposerMessage {
                command_id: &command_id,
                thread_id: &thread_id,
                message_id: &message_id,
                text: &outgoing.text,
                title_seed: &outgoing.title,
                model_selection: &selection,
                dispatch,
                context: outgoing.context.as_ref(),
                attachments: &outgoing.attachments,
            };
            steps.push((commands::composer_message(&message), "Failed to send message."));
        }
        let failures: Vec<&'static str> = steps.iter().map(|(_, failure)| *failure).collect();
        let request = self.state.read(cx).dispatch_in_order(steps.into_iter().map(|(command, _)| command).collect());
        let target = Target::Thread(thread_id);
        let taken = self.begin_send(&target, &outgoing.prompt, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let failure = request.await.err().map(|(index, error)| format!("{} {error}", failures.get(index).copied().unwrap_or("Failed to send message.")));
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                if let Some(error) = failure {
                    this.restore(&target, &outgoing.prompt, taken, error, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Clears the prompt and takes the attachments and the context `prompt` links, as the web
    /// clears the composer on send. Whatever is added while the send is in flight stays.
    fn begin_send(&mut self, target: &Target, prompt: &str, window: &mut Window, cx: &mut Context<Self>) -> Taken {
        self.sending = true;
        self.error = None;
        self.set_prompt("", window, cx);
        cx.notify();
        Taken {
            attachments: self.take_attachments(target),
            threads: self.take_threads(target, prompt),
            terminals: self.take_terminals(target, prompt),
        }
    }

    /// A failed send puts the text back unless the user already typed something new, its
    /// attachments back in front of any added since, and its context beside what was added since.
    fn restore(
        &mut self,
        target: &Target,
        text: &str,
        taken: Taken,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prompt(cx).trim().is_empty() {
            self.set_prompt(text, window, cx);
        }
        self.restore_attachments(target, taken.attachments);
        self.restore_threads(target, taken.threads);
        self.restore_terminals(target, taken.terminals);
        self.error = Some(error);
    }

    pub(super) fn respond_to_approval(&mut self, request_id: String, decision: ApprovalDecision, cx: &mut Context<Self>) {
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

    pub(super) fn run_request(&mut self, request_id: String, command: Value, failure: &'static str, cx: &mut Context<Self>) {
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
