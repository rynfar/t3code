//! "Fork from this response" (`AssistantForkButton` in `TimelineAssistantRows.tsx`,
//! `canForkProjectedAssistantItem` in `client-runtime/src/state/threadWorkflows.ts`, the item's
//! provider session from `resolveV2ItemSupport` in `itemSupport.ts`, `forkThreadFromRun` in
//! `operations/commands.ts`, `onForkFromRun` in `useTimelineRewind.ts`).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::commands::CREATION_SOURCE;
use crate::thread::ThreadProjection;
use crate::turn_item::{TurnItem, TurnItemKind};

pub const FORK_LABEL: &str = "Fork from this response";
pub const FORK_FAILED: &str = "Failed to fork this response.";

/// Whether each provider thread whose session records capabilities can fork: native
/// fork-from-turn with strong thread ids, or full-thread handoff. Threads without that evidence
/// are absent, and fork through the server's portable fallback.
pub fn fork_support(projection: &ThreadProjection) -> HashMap<String, bool> {
    projection
        .provider_threads
        .iter()
        .filter_map(|thread| {
            let session_id = thread.value.get("providerSessionId")?.as_str()?;
            let capabilities = projection.provider_sessions.iter().find(|session| session.id == session_id)?.value.get("capabilities")?;
            let flag = |section: &str, key: &str| capabilities.get(section).and_then(|section| section.get(key)).and_then(Value::as_bool) == Some(true);
            let native = flag("threads", "canForkThread")
                && flag("threads", "canForkFromTurn")
                && capabilities.get("identity").and_then(|identity| identity.get("nativeThreadIds")).and_then(Value::as_str) == Some("strong");
            Some((thread.id.clone(), native || flag("context", "supportsFullThreadHandoff")))
        })
        .collect()
}

/// A completed assistant message in a run can fork unless its provider is known not to.
pub fn can_fork(item: &TurnItem, support: &HashMap<String, bool>) -> bool {
    matches!(item.kind, TurnItemKind::AssistantMessage { .. })
        && item.run_id.is_some()
        && item.status == "completed"
        && item.provider_thread_id.as_ref().and_then(|thread| support.get(thread)).is_none_or(|capable| *capable)
}

/// `thread.fork` from a run into a new thread titled `<title> fork`.
pub fn fork_from_run(command_id: &str, source_thread_id: &str, target_thread_id: &str, run_id: &str, source_title: &str) -> Value {
    json!({
        "type": "thread.fork",
        "commandId": command_id,
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
        "sourceThreadId": source_thread_id,
        "targetThreadId": target_thread_id,
        "sourcePoint": { "type": "run", "runId": run_id },
        "title": format!("{source_title} fork"),
    })
}
