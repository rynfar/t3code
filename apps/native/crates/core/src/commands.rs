//! RPC method names and command payloads, as `packages/contracts` defines them.
//! The native client identifies as the desktop surface and creates work with
//! `creationSource: "web"`, which is what the desktop web view sends.

use serde_json::{Value, json};

pub mod method {
    pub const SUBSCRIBE_SERVER_CONFIG: &str = "subscribeServerConfig";
    pub const SERVER_PROBE: &str = "server.probe";
    pub const SUBSCRIBE_SHELL: &str = "orchestration.subscribeShell";
    pub const SUBSCRIBE_THREAD: &str = "orchestration.subscribeThread";
    pub const DISPATCH_COMMAND: &str = "orchestration.dispatchCommand";
}

pub const CREATION_SOURCE: &str = "web";

/// Asks for the `synchronized` marker so "no rows" can mean empty rather than still loading.
pub fn subscribe_shell(after_sequence: Option<u64>) -> Value {
    let mut payload = json!({ "requestCompletionMarker": true });
    if let Some(sequence) = after_sequence {
        payload["afterSequence"] = json!(sequence);
    }
    payload
}

pub fn subscribe_thread(thread_id: &str, after_sequence: Option<u64>) -> Value {
    let mut payload = json!({ "threadId": thread_id, "requestCompletionMarker": true });
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

pub fn stop_thread(command_id: &str, thread_id: &str) -> Value {
    json!({ "type": "thread.stop", "commandId": command_id, "threadId": thread_id })
}

/// Commands that take only `commandId` and `threadId`: archive, unarchive, pin, unpin, settle…
pub fn simple_thread_command(kind: &str, command_id: &str, thread_id: &str) -> Value {
    json!({ "type": kind, "commandId": command_id, "threadId": thread_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_message_matches_message_dispatch_schema() {
        let command = send_message("c1", "t1", "hi", &DispatchMode::StartImmediately);
        assert_eq!(command["type"], "message.dispatch");
        assert_eq!(command["messageId"], "c1");
        assert_eq!(command["dispatchMode"], json!({ "type": "start_immediately" }));
        assert_eq!(command["creationSource"], "web");
        let steer = send_message("c2", "t1", "x", &DispatchMode::SteerActive { target_run_id: "r1".into() });
        assert_eq!(steer["dispatchMode"], json!({ "type": "steer_active", "targetRunId": "r1" }));
    }
}
