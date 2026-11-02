//! What Send does (`useComposerSend.ts`): launch a draft as a new thread (`startThreadTurn` with
//! `bootstrap.createThread`, in a worktree when the project's default asks for one), or send to a
//! thread after declining what it waits on and persisting permissions and mode, each step after
//! the previous one committed, as `declinePendingApprovals` and `persistThreadSettingsForNextTurn`
//! await them.

use gpui::{Context, Window};
use serde_json::Value;
use t3_core::commands::{self, ApprovalDecision, ComposerMessage, LaunchThread, method};
use t3_core::composer::{self as rules, FollowUpBehavior, SubmitIntent};
use t3_core::model::ModelSelection;
use t3_core::server_config::ThreadEnvMode;
use t3_core::traits;
use t3_core::workspace::{self, VcsRefs};

use super::{Composer, Resolved, Target};
use crate::app_state::uuid_v4;

const NO_PROVIDER: &str = "Enable a provider in Settings to send a message";

impl Composer {
    /// Submit, as the send button and Enter do.
    pub(super) fn submit(&mut self, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The model selection a turn carries, with the options `getComposerProviderState` dispatches.
    fn dispatch_selection(resolved: &Resolved) -> Option<ModelSelection> {
        let model = resolved.model.as_ref()?;
        Some(traits::model_selection(&model.instance_id, &model.model, &resolved.dispatch_options))
    }

    fn launch(&mut self, project_id: String, text: String, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
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
        let context = self.message_context(&text);
        let project_file = state.rpc(method::PROJECTS_READ_FILE, workspace::project_file_request(&cwd));
        let refs = state.rpc(method::VCS_LIST_REFS, workspace::list_refs_request(&cwd));
        self.begin_send(window, cx);
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
                let title = rules::title_seed(&text);
                let payload = commands::launch_thread(&LaunchThread {
                    command_id: &command_id,
                    thread_id: &thread_id,
                    project_id: &project_id,
                    title: &title,
                    model_selection: &selection,
                    runtime_mode: resolved.runtime_mode,
                    interaction_mode: resolved.interaction_mode,
                    workspace: &strategy,
                    message_id: &message_id,
                    text: &text,
                    context: context.as_ref(),
                    attachments: &[],
                });
                Ok::<_, String>((thread_id, this.state.read(cx).rpc(method::LAUNCH_THREAD, payload)))
            });
            let result = match launch {
                Ok(Ok((thread_id, request))) => request.await.map(|_| thread_id).map_err(|error| error.to_string()),
                Ok(Err(error)) => Err(error),
                Err(_) => return,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                if result.is_ok() {
                    this.clear_mentions(&Target::Draft(project_id.clone()));
                }
                match result {
                    Ok(_) if intent == SubmitIntent::Background => {}
                    Ok(thread_id) => this.state.update(cx, |state, cx| state.select_when_listed(&thread_id, cx)),
                    Err(error) => this.restore(&text, error, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send(&mut self, thread_id: String, text: String, intent: SubmitIntent, window: &mut Window, cx: &mut Context<Self>) {
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
            let context = self.message_context(&text);
            let message = ComposerMessage {
                command_id: &command_id,
                thread_id: &thread_id,
                message_id: &message_id,
                text: &text,
                title_seed: &title,
                model_selection: &selection,
                dispatch,
                context: context.as_ref(),
                attachments: &[],
            };
            steps.push((commands::composer_message(&message), "Failed to send message."));
        }
        let failures: Vec<&'static str> = steps.iter().map(|(_, failure)| *failure).collect();
        let request = self.state.read(cx).dispatch_in_order(steps.into_iter().map(|(command, _)| command).collect());
        self.begin_send(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let failure = request.await.err().map(|(index, error)| format!("{} {error}", failures.get(index).copied().unwrap_or("Failed to send message.")));
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match failure {
                    Some(error) => this.restore(&text, error, window, cx),
                    None => this.clear_mentions(&Target::Thread(thread_id)),
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
