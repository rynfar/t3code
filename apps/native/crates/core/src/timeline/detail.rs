//! Withheld tool output (`outputOmitted`) fetched on demand with `orchestration.getTurnItem`:
//! `turnItemNeedsDetailFetch`, `turnItemDetailRevision` and `commandOutputText` from
//! `packages/client-runtime/src/work-log/itemDetail.ts`.

use serde_json::{Value, json};

use crate::turn_item::{TurnItem, TurnItemKind};
use crate::wire;

pub const GET_TURN_ITEM: &str = "orchestration.getTurnItem";

/// `turnItemNeedsDetailFetch`.
pub fn needs_detail_fetch(item: &TurnItem) -> bool {
    match &item.kind {
        TurnItemKind::CommandExecution { output_omitted, .. } => *output_omitted,
        TurnItemKind::DynamicTool { output_omitted, input, .. } => {
            *output_omitted || (input.get("truncated") == Some(&Value::Bool(true)) && input.get("summary").is_some_and(Value::is_string))
        }
        _ => false,
    }
}

/// `turnItemDetailRevision`: a running item keeps one key, so an open row fetches once while it
/// streams and again when it finishes.
pub fn detail_revision(item: &TurnItem) -> String {
    match item.status.as_str() {
        "idle" | "pending" | "running" | "waiting" => "live".into(),
        _ => item.updated_at.clone(),
    }
}

pub fn detail_request(item: &TurnItem) -> Value {
    json!({ "threadId": item.thread_id, "itemId": item.id, "revision": detail_revision(item) })
}

/// The fetched item, `None` when the server no longer has it.
pub fn decode_detail(mut value: Value) -> Result<Option<TurnItem>, serde_json::Error> {
    if let Some(ids) = wire::take_id_table(&mut value)
        && let Some(object) = value.as_object_mut()
    {
        wire::expand_record_ids(object, &ids);
    }
    match value.get_mut("item").map(Value::take) {
        None | Some(Value::Null) => Ok(None),
        Some(item) => serde_json::from_value::<TurnItem>(item).map(|item| item.is_known().then_some(item)),
    }
}

/// `commandOutputText`: Claude's `{"stdout","stderr","interrupted"}` envelope as plain text.
pub fn command_output_text(output: &str) -> Option<String> {
    if output.trim().is_empty() {
        return None;
    }
    if !output.trim_start().starts_with("{\"stdout\"") {
        return Some(output.to_owned());
    }
    let parsed: Value = serde_json::from_str(output.trim()).ok()?;
    let (Some(stdout), Some(stderr), Some(_)) =
        (parsed.get("stdout").and_then(Value::as_str), parsed.get("stderr").and_then(Value::as_str), parsed.get("interrupted").and_then(Value::as_bool))
    else {
        return Some(output.to_owned());
    };
    let text = [stdout, stderr].into_iter().filter(|part| !part.trim().is_empty()).collect::<Vec<_>>().join("\n");
    (!text.is_empty()).then_some(text)
}
