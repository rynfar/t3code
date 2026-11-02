//! Undoes the opt-in wire compactions the server applies for clients that ask for them.
//! Ports `expandRecordIds` from `contracts/src/orchestrationV2IdTable.ts`, the restore half of
//! `shared/src/orchestrationV2DedupedBodies.ts`, and `restoreLocalVisibleTurnItems` from
//! `shared/src/orchestrationV2BoundedSnapshot.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::thread::{Entity, ProjectedTurnItem, ThreadProjection, Visibility};
use crate::turn_item::{CheckpointFileSummary, TurnItem, TurnItemKind};

fn is_id_key(key: &str) -> bool {
    key == "id" || key.ends_with("Id")
}

fn is_id_list_key(key: &str) -> bool {
    key.ends_with("Ids")
}

/// Removes the response's `ids` table, if it carries one.
pub fn take_id_table(raw: &mut Value) -> Option<Vec<Value>> {
    let object = raw.as_object_mut()?;
    if !object.get("ids").is_some_and(Value::is_array) {
        return None;
    }
    match object.remove("ids") {
        Some(Value::Array(ids)) => Some(ids),
        _ => None,
    }
}

fn lookup(ids: &[Value], index: &Value) -> Value {
    index.as_u64().and_then(|i| ids.get(i as usize)).cloned().unwrap_or(Value::Null)
}

fn expand_record(record: &mut Map<String, Value>, ids: &[Value]) {
    for (key, value) in record.iter_mut() {
        match value {
            Value::Number(_) if is_id_key(key) => *value = lookup(ids, value),
            Value::Array(entries) if is_id_list_key(key) => {
                for entry in entries.iter_mut().filter(|entry| entry.is_number()) {
                    *entry = lookup(ids, entry);
                }
            }
            Value::Object(item) if key == "item" => expand_record(item, ids),
            _ => {}
        }
    }
}

/// Each top-level array element and top-level object of `container` is a record.
pub fn expand_record_ids(container: &mut Map<String, Value>, ids: &[Value]) {
    for value in container.values_mut() {
        match value {
            Value::Array(records) => {
                for record in records.iter_mut() {
                    if let Value::Object(record) = record {
                        expand_record(record, ids);
                    }
                }
            }
            Value::Object(record) => expand_record(record, ids),
            _ => {}
        }
    }
}

/// Where a deduped body is copied back from: the records the client already holds.
pub trait BodySource {
    fn message_text(&self, id: &str) -> Option<String>;
    fn checkpoint_files(&self, id: &str) -> Option<Vec<CheckpointFileSummary>>;
    fn subagent(&self, id: &str) -> Option<&Value>;
}

fn checkpoint_files(entity: &Entity) -> Option<Vec<CheckpointFileSummary>> {
    serde_json::from_value(entity.value.get("files")?.clone()).ok()
}

fn message_text(entity: &Entity) -> Option<String> {
    entity.value.get("text").and_then(Value::as_str).map(str::to_owned)
}

/// `latestBodies`: a deduped item repeats a record the stream just sent, so search newest first.
pub struct LatestBodies<'a> {
    pub messages: &'a [Entity],
    pub checkpoints: &'a [Entity],
    pub subagents: &'a [Entity],
}

impl BodySource for LatestBodies<'_> {
    fn message_text(&self, id: &str) -> Option<String> {
        self.messages.iter().rev().find(|m| m.id == id).and_then(message_text)
    }

    fn checkpoint_files(&self, id: &str) -> Option<Vec<CheckpointFileSummary>> {
        self.checkpoints.iter().rev().find(|c| c.id == id).and_then(checkpoint_files)
    }

    fn subagent(&self, id: &str) -> Option<&Value> {
        self.subagents.iter().rev().find(|s| s.id == id).map(|s| &s.value)
    }
}

/// `snapshotBodySource`: indexes by id, keeping the last record for a repeated id.
struct SnapshotBodies<'a> {
    messages: HashMap<&'a str, &'a Entity>,
    checkpoints: HashMap<&'a str, &'a Entity>,
    subagents: HashMap<&'a str, &'a Value>,
}

impl<'a> SnapshotBodies<'a> {
    fn new(messages: &'a [Entity], checkpoints: &'a [Entity], subagents: &'a [Entity]) -> Self {
        SnapshotBodies {
            messages: messages.iter().map(|e| (e.id.as_str(), e)).collect(),
            checkpoints: checkpoints.iter().map(|e| (e.id.as_str(), e)).collect(),
            subagents: subagents.iter().map(|e| (e.id.as_str(), &e.value)).collect(),
        }
    }
}

impl BodySource for SnapshotBodies<'_> {
    fn message_text(&self, id: &str) -> Option<String> {
        self.messages.get(id).copied().and_then(message_text)
    }

    fn checkpoint_files(&self, id: &str) -> Option<Vec<CheckpointFileSummary>> {
        self.checkpoints.get(id).copied().and_then(checkpoint_files)
    }

    fn subagent(&self, id: &str) -> Option<&Value> {
        self.subagents.get(id).copied()
    }
}

fn marked(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

/// `restoreTurnItemBodies`. A marked body whose record is missing stays blank and marked.
pub fn restore_turn_item_bodies(item: &mut TurnItem, source: &dyn BodySource) {
    match &mut item.kind {
        TurnItemKind::UserMessage { message_id, text, .. }
        | TurnItemKind::AssistantMessage { message_id, text, .. } => {
            if item.text_omitted
                && let Some(restored) = source.message_text(message_id)
            {
                *text = restored;
                item.text_omitted = false;
            }
        }
        TurnItemKind::Checkpoint { checkpoint_id, files, .. } => {
            if item.files_omitted
                && let Some(restored) = source.checkpoint_files(checkpoint_id)
            {
                *files = restored;
                item.files_omitted = false;
            }
        }
        TurnItemKind::Subagent { subagent_id, prompt, result, .. } => {
            let Some(previous) = source.subagent(subagent_id) else { return };
            if item.prompt_omitted && !marked(previous, "promptOmitted") {
                *prompt = previous.get("prompt").and_then(Value::as_str).unwrap_or_default().to_owned();
                item.prompt_omitted = false;
            }
            if item.result_omitted && !marked(previous, "resultOmitted") {
                *result = previous.get("result").and_then(Value::as_str).map(str::to_owned);
                item.result_omitted = false;
            }
        }
        _ => {}
    }
}

fn has_marker(item: &TurnItem) -> bool {
    item.text_omitted || item.files_omitted || item.prompt_omitted || item.result_omitted
}

/// `restoreSubagentBodies` for a `subagent.updated` record.
pub fn restore_subagent_record(value: &mut Value, previous: Option<&Value>) {
    let Some(previous) = previous else { return };
    for (marker, field) in [("promptOmitted", "prompt"), ("resultOmitted", "result")] {
        if marked(value, marker) && !marked(previous, marker)
            && let Some(object) = value.as_object_mut()
        {
            object.insert(field.into(), previous.get(field).cloned().unwrap_or(Value::Null));
            object.remove(marker);
        }
    }
}

fn is_local_row(thread_id: &str, row: &ProjectedTurnItem) -> bool {
    row.visibility == Visibility::Local || row.source_thread_id == thread_id
}

/// `boundedSnapshotProjection`: restores deduped bodies and, when the server omitted them,
/// the turn items of local visible rows, which lead the full list. Restored rows share their
/// item with `turn_items`.
pub fn restore_snapshot(projection: &mut ThreadProjection, turn_items_omit_local_visible: bool) {
    let ThreadProjection { thread, messages, checkpoints, subagents, turn_items, visible_turn_items, .. } = projection;
    let marked_rows = visible_turn_items.iter().any(|row| is_local_row(&thread.id, row) && has_marker(&row.item));
    if marked_rows || turn_items.iter().any(|item| has_marker(item)) {
        let source = SnapshotBodies::new(messages, checkpoints, subagents);
        let rows = visible_turn_items.iter_mut().filter(|row| is_local_row(&thread.id, row));
        for item in rows.map(|row| &mut row.item).chain(turn_items.iter_mut()) {
            if has_marker(item) {
                restore_turn_item_bodies(Arc::make_mut(item), &source);
            }
        }
    }
    if turn_items_omit_local_visible {
        let local: Vec<Arc<TurnItem>> = visible_turn_items
            .iter()
            .filter(|row| is_local_row(&thread.id, row))
            .map(|row| row.item.clone())
            .collect();
        if !local.is_empty() {
            turn_items.splice(0..0, local);
        }
    }
}

#[cfg(test)]
mod tests;
