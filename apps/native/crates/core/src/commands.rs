//! RPC method names and command payloads, as `packages/contracts` defines them and
//! `client-runtime/src/operations/commands.ts` sends them. The native client identifies as the
//! desktop surface and creates work with `creationSource: "web"`, which is what the desktop web
//! view sends.

use serde_json::{Map, Value, json};

use crate::model::ModelSelection;
use crate::server_config::{InteractionMode, RuntimeMode};
use crate::workspace::Strategy;

pub mod method {
    pub const SUBSCRIBE_SERVER_CONFIG: &str = "subscribeServerConfig";
    pub const SERVER_PROBE: &str = "server.probe";
    pub const SERVER_UPDATE_SETTINGS: &str = "server.updateSettings";
    pub const SUBSCRIBE_SHELL: &str = "orchestration.subscribeShell";
    pub const SUBSCRIBE_THREAD: &str = "orchestration.subscribeThread";
    pub const DISPATCH_COMMAND: &str = "orchestration.dispatchCommand";
    pub const LAUNCH_THREAD: &str = "orchestration.launchThread";
    pub const PROJECTS_READ_FILE: &str = "projects.readFile";
    pub const VCS_LIST_REFS: &str = "vcs.listRefs";
}

pub const CREATION_SOURCE: &str = "web";

/// Per-cwd provider snapshots are left out: the native composer has no slash-command menu yet,
/// and `server.refreshProviders` fetches them on demand when it does. Limit sources feed the
/// Usage page's pooled limits, as the web opts in.
pub fn subscribe_server_config() -> Value {
    json!({ "workspaceSnapshotsOnDemand": true, "usageLimitSources": true })
}

/// Which rows the shell may leave to on-demand reads, as `shell.ts` decides from the config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShellOptions {
    /// Idle subagent children come from `getThreadFamily`.
    pub omit_idle_subagents: bool,
    /// Older settled threads are counted in `olderSettled` and come from `getOlderSettledThreads`.
    pub omit_older_settled: bool,
}

impl ShellOptions {
    /// Older settled rows need families too, so a server must advertise both.
    pub fn for_server(shell_thread_families: bool, shell_older_settled: bool) -> ShellOptions {
        ShellOptions {
            omit_idle_subagents: shell_thread_families,
            omit_older_settled: shell_older_settled && shell_thread_families,
        }
    }
}

/// Asks for the `synchronized` marker so "no rows" can mean empty rather than still loading.
pub fn subscribe_shell(after_sequence: Option<u64>, options: ShellOptions) -> Value {
    let mut payload = json!({ "requestCompletionMarker": true });
    if let Some(sequence) = after_sequence {
        payload["afterSequence"] = json!(sequence);
    }
    if options.omit_idle_subagents {
        payload["omitIdleSubagents"] = json!(true);
    }
    if options.omit_older_settled {
        payload["omitOlderSettled"] = json!(true);
    }
    payload
}

/// Opts into what `thread::decode_thread_stream_item` undoes, as `threads.ts` does when it can
/// page history: a bounded window, omitted local turn items, deduped bodies, an ID table.
/// Collapsed work (`acceptDeferredWork`) needs a transcript control the native app lacks.
pub fn subscribe_thread(thread_id: &str, after_sequence: Option<u64>) -> Value {
    let mut payload = json!({
        "threadId": thread_id,
        "requestCompletionMarker": true,
        "acceptBoundedSnapshot": true,
        "acceptCompactTurnItems": true,
        "acceptCompactIds": true,
        "acceptDedupedBodies": true,
    });
    if let Some(sequence) = after_sequence {
        payload["afterSequence"] = json!(sequence);
    }
    payload
}

/// How a new message relates to a run already in flight. Mirrors `message.dispatch.dispatchMode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchMode {
    StartImmediately,
    QueueAfterActive,
    SteerActive { target_run_id: String },
}

impl DispatchMode {
    fn to_value(&self) -> Value {
        match self {
            DispatchMode::StartImmediately => json!({ "type": "start_immediately" }),
            DispatchMode::QueueAfterActive => json!({ "type": "queue_after_active" }),
            DispatchMode::SteerActive { target_run_id } => {
                json!({ "type": "steer_active", "targetRunId": target_run_id })
            }
        }
    }
}

pub fn send_message(command_id: &str, thread_id: &str, text: &str, mode: &DispatchMode) -> Value {
    json!({
        "type": "message.dispatch",
        "commandId": command_id,
        "threadId": thread_id,
        "messageId": command_id,
        "text": text,
        "attachments": [],
        "dispatchMode": mode.to_value(),
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
    })
}

/// `ComposerDispatchMode` after `planComposerTurn`: what a send does to a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerDispatch {
    Auto,
    Queue,
    Steer,
}

/// The composer's send to a server thread, as `startThreadTurn` builds it.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposerMessage<'a> {
    pub command_id: &'a str,
    pub thread_id: &'a str,
    pub message_id: &'a str,
    pub text: &'a str,
    pub title_seed: &'a str,
    pub model_selection: &'a ModelSelection,
    pub dispatch: ComposerDispatch,
    /// `OrchestrationMessageContext`: the records the text references.
    pub context: Option<&'a Value>,
    /// Persisted `ChatAttachment`s.
    pub attachments: &'a [Value],
}

pub fn composer_message(message: &ComposerMessage) -> Value {
    let mut command = json!({
        "type": "message.dispatch",
        "commandId": message.command_id,
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
        "threadId": message.thread_id,
        "messageId": message.message_id,
        "text": message.text,
        "attachments": message.attachments,
        "titleSeed": message.title_seed,
        "modelSelection": message.model_selection,
    });
    if let Some(context) = message.context {
        command["context"] = context.clone();
    }
    match message.dispatch {
        ComposerDispatch::Queue => command["dispatchMode"] = json!({ "type": "queue_after_active" }),
        ComposerDispatch::Auto | ComposerDispatch::Steer => {
            let intent = if message.dispatch == ComposerDispatch::Steer { "steer" } else { "auto" };
            command["deliveryIntent"] = json!(intent);
            command["dispatchMode"] = json!({ "type": "start_immediately" });
        }
    }
    command
}

pub fn stop_thread(command_id: &str, thread_id: &str) -> Value {
    json!({ "type": "thread.stop", "commandId": command_id, "threadId": thread_id })
}

/// What servers without the `threadStop` capability take: stop the run and hold its queue.
pub fn interrupt_run(command_id: &str, thread_id: &str, run_id: &str) -> Value {
    json!({ "type": "run.interrupt", "commandId": command_id, "threadId": thread_id, "runId": run_id, "holdQueue": true })
}

/// Commands that take only `commandId` and `threadId`: archive, unarchive, pin, unpin, settle…
pub fn simple_thread_command(kind: &str, command_id: &str, thread_id: &str) -> Value {
    json!({ "type": kind, "commandId": command_id, "threadId": thread_id })
}

pub fn set_runtime_mode(command_id: &str, thread_id: &str, mode: RuntimeMode) -> Value {
    json!({ "type": "thread.runtime-mode.set", "commandId": command_id, "threadId": thread_id, "runtimeMode": mode })
}

pub fn set_interaction_mode(command_id: &str, thread_id: &str, mode: InteractionMode) -> Value {
    json!({
        "type": "thread.interaction-mode.set",
        "commandId": command_id,
        "threadId": thread_id,
        "interactionMode": mode,
    })
}

pub fn set_model_selection(command_id: &str, thread_id: &str, selection: &ModelSelection) -> Value {
    json!({
        "type": "thread.model-selection.set",
        "commandId": command_id,
        "threadId": thread_id,
        "modelSelection": selection,
    })
}

/// `ProviderApprovalDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalDecision::Accept => "accept",
            ApprovalDecision::AcceptForSession => "acceptForSession",
            ApprovalDecision::AcceptAlways => "acceptAlways",
            ApprovalDecision::Decline => "decline",
            ApprovalDecision::Cancel => "cancel",
        }
    }

    pub fn parse(value: &str) -> Option<ApprovalDecision> {
        [Self::Accept, Self::AcceptForSession, Self::AcceptAlways, Self::Decline, Self::Cancel]
            .into_iter()
            .find(|decision| decision.as_str() == value)
    }
}

/// `feedback` declines with a message to the agent, for live requests that support it.
pub fn respond_to_approval(
    command_id: &str,
    thread_id: &str,
    request_id: &str,
    decision: ApprovalDecision,
    feedback: Option<&str>,
) -> Value {
    let mut command = json!({
        "type": "runtime-request.respond",
        "commandId": command_id,
        "threadId": thread_id,
        "requestId": request_id,
        "decision": decision.as_str(),
    });
    if let Some(feedback) = feedback {
        command["feedback"] = json!(feedback);
    }
    command
}

pub fn respond_to_user_input(command_id: &str, thread_id: &str, request_id: &str, answers: Map<String, Value>) -> Value {
    json!({
        "type": "runtime-request.respond",
        "commandId": command_id,
        "threadId": thread_id,
        "requestId": request_id,
        "answers": answers,
    })
}

pub fn dismiss_user_input(command_id: &str, thread_id: &str, request_id: &str) -> Value {
    json!({ "type": "thread.user-input.dismiss", "commandId": command_id, "threadId": thread_id, "requestId": request_id })
}

/// `OrchestrationV2ThreadLaunchInput` for a draft's first send, as `startThreadTurn` builds it
/// with `bootstrap.createThread` and, for a new worktree, `bootstrap.prepareWorktree`.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchThread<'a> {
    pub command_id: &'a str,
    pub thread_id: &'a str,
    pub project_id: &'a str,
    pub title: &'a str,
    pub model_selection: &'a ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: &'a Strategy,
    pub message_id: &'a str,
    pub text: &'a str,
    pub context: Option<&'a Value>,
    pub attachments: &'a [Value],
}

pub fn launch_thread(launch: &LaunchThread) -> Value {
    let mut payload = json!({
        "commandId": launch.command_id,
        "creationSource": CREATION_SOURCE,
        "threadId": launch.thread_id,
        "projectId": launch.project_id,
        "title": launch.title,
        "generateTitle": true,
        "modelSelection": launch.model_selection,
        "runtimeMode": launch.runtime_mode,
        "interactionMode": launch.interaction_mode,
        "workspaceStrategy": launch.workspace.to_json(),
        "initialMessage": {
            "messageId": launch.message_id,
            "text": launch.text,
            "attachments": launch.attachments,
        },
    });
    if let Some(context) = launch.context {
        payload["initialMessage"]["context"] = context.clone();
    }
    payload
}

/// `server.updateSettings` with `setModelFavorites`, as `useToggleEnvironmentModelFavorite` saves.
pub fn set_model_favorite(provider: &str, model: &str, favorite: bool) -> Value {
    json!({ "patch": { "setModelFavorites": [{ "provider": provider, "model": model, "favorite": favorite }] } })
}

#[cfg(test)]
mod tests;
