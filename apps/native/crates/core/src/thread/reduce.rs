//! `applyOrchestrationV2ProjectionEvent` from `client-runtime/src/state/orchestrationV2Projection.ts`,
//! with the visibility rules from `shared/src/orchestrationV2Timeline.ts`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::{ApplyOptions, DomainEvent, Entity, EventBody, ProjectedTurnItem, RunAttempt, ThreadProjection, Visibility};
use crate::turn_item::{TurnItem, TurnItemKind};
use crate::wire::{self, LatestBodies};

/// Applies one domain event in place. Returns true when the projection changed.
pub fn apply_event(projection: &mut ThreadProjection, event: DomainEvent, options: ApplyOptions) -> bool {
    if event.thread_id != projection.thread.id {
        return false;
    }
    // A turn item bumps activity only once it is applied: a dropped partial update changes nothing.
    let touches_activity = !matches!(
        event.body,
        EventBody::ThreadTouched(_) | EventBody::Unknown(_) | EventBody::TurnItemUpserted(_)
    );
    if touches_activity {
        projection.updated_at = event.occurred_at.clone();
    }
    match event.body {
        EventBody::ThreadReplaced(thread) | EventBody::ThreadTouched(thread) => {
            projection.thread = *thread;
        }
        // Pull request links are not modeled natively yet; the activity bump above still applies.
        EventBody::ThreadPullRequestLinkUpdated | EventBody::CheckpointRollbackRequested => {}
        EventBody::RunUpserted(run) => {
            upsert_by(&mut projection.runs, *run, |r| &r.id);
            refresh_visible_items(projection);
        }
        EventBody::RunBackgroundWorkCancelled { run_id, restart_cancelled_background_work } => {
            if let Some(run) = projection.runs.iter_mut().find(|run| run.id == run_id) {
                run.restart_cancelled_background_work = restart_cancelled_background_work;
            }
        }
        EventBody::AttemptUpserted(attempt) => {
            upsert_by(&mut projection.attempts, attempt, |a| &a.id);
            refresh_visible_items(projection);
        }
        EventBody::NodeUpserted(entity) => upsert_entity(&mut projection.nodes, entity),
        EventBody::SubagentUpserted(mut entity) => {
            let previous = projection.subagents.iter().find(|s| s.id == entity.id).map(|s| &s.value);
            wire::restore_subagent_record(&mut entity.value, previous);
            upsert_entity(&mut projection.subagents, entity);
        }
        EventBody::ProviderSessionUpserted(entity) => {
            upsert_entity(&mut projection.provider_sessions, entity)
        }
        EventBody::ProviderSessionDetached { provider_session_id } => {
            projection.provider_sessions.retain(|session| session.id != provider_session_id);
        }
        EventBody::ProviderThreadUpserted(entity) => {
            upsert_entity(&mut projection.provider_threads, entity)
        }
        EventBody::ProviderTurnUpserted(mut entity) => {
            // A turn update without tokenUsage keeps the last one reported.
            if entity.value.get("tokenUsage").is_none_or(Value::is_null) {
                let previous = projection
                    .provider_turns
                    .iter()
                    .find(|turn| turn.id == entity.id)
                    .and_then(|turn| turn.value.get("tokenUsage"))
                    .filter(|usage| !usage.is_null())
                    .cloned();
                if let (Some(usage), Some(object)) = (previous, entity.value.as_object_mut()) {
                    object.insert("tokenUsage".into(), usage);
                }
            }
            upsert_entity(&mut projection.provider_turns, entity);
        }
        EventBody::RuntimeRequestUpserted(entity) => {
            upsert_entity(&mut projection.runtime_requests, entity)
        }
        EventBody::MessageUpserted(entity) => upsert_entity(&mut projection.messages, entity),
        EventBody::PlanUpserted(entity) => upsert_entity(&mut projection.plans, entity),
        EventBody::TurnItemUpserted(item) => {
            return apply_turn_item(projection, item, event.occurred_at, options);
        }
        EventBody::CheckpointScopeUpserted(entity) => {
            upsert_entity(&mut projection.checkpoint_scopes, entity)
        }
        EventBody::CheckpointCaptured(entity) => upsert_entity(&mut projection.checkpoints, entity),
        EventBody::ContextHandoffUpserted(entity) => {
            upsert_entity(&mut projection.context_handoffs, entity)
        }
        EventBody::ContextTransferUpserted(entity) => {
            upsert_entity(&mut projection.context_transfers, entity)
        }
        EventBody::Unknown(_) => return false,
    }
    true
}

fn apply_turn_item(
    projection: &mut ThreadProjection,
    mut item: Arc<TurnItem>,
    occurred_at: String,
    options: ApplyOptions,
) -> bool {
    if item.text_omitted || item.files_omitted || item.prompt_omitted || item.result_omitted {
        let source = LatestBodies {
            messages: &projection.messages,
            checkpoints: &projection.checkpoints,
            subagents: &projection.subagents,
        };
        wire::restore_turn_item_bodies(Arc::make_mut(&mut item), &source);
    }
    // Streamed updates land on the newest items, so search from the end.
    let index = projection.turn_items.iter().rposition(|candidate| candidate.id == item.id);
    if options.partial_timeline
        && index.is_none()
        && should_drop_missing_partial_item(projection, &item, options.latest_local_turn_ordinal)
    {
        return false;
    }
    projection.updated_at = occurred_at;
    let previous_was_interrupt = index
        .map(|i| matches!(projection.turn_items[i].kind, TurnItemKind::RunInterruptRequest { .. }))
        .unwrap_or(false);
    match index {
        Some(i) => projection.turn_items[i] = item.clone(),
        None => projection.turn_items.push(item.clone()),
    }
    // Only interrupt requests can change another item's visibility. Streaming text/tool updates
    // must not recheck every row against every run.
    if previous_was_interrupt || matches!(item.kind, TurnItemKind::RunInterruptRequest { .. }) {
        refresh_visible_items(projection);
    }
    if is_item_visible(projection, &item) {
        upsert_visible_item(projection, item, options);
    } else {
        remove_visible_item(&mut projection.visible_turn_items, &item.id);
    }
    true
}

fn upsert_entity(items: &mut Vec<Entity>, entity: Entity) {
    upsert_by(items, entity, |e| &e.id);
}

fn upsert_by<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|candidate| id(candidate) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

fn renumber(rows: &mut [ProjectedTurnItem]) {
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position;
    }
}

fn remove_visible_item(rows: &mut Vec<ProjectedTurnItem>, source_item_id: &str) {
    if let Some(index) = rows.iter().position(|row| row.source_item_id == source_item_id) {
        rows.remove(index);
        renumber(rows);
    }
}

/// `isOrchestrationV2TurnItemVisible`.
pub(crate) fn is_item_visible(projection: &ThreadProjection, item: &TurnItem) -> bool {
    let run_status = item
        .run_id
        .as_deref()
        .and_then(|run_id| projection.runs.iter().find(|run| run.id == run_id))
        .map(|run| run.status.as_str());
    if run_status == Some("rolled_back") {
        return false;
    }
    // Cancelled queued input never reached the provider and must stay out of the transcript.
    if run_status == Some("cancelled") && item.is_queued_user_message() {
        return false;
    }
    !is_superseded_interrupt(item, &projection.attempts, |run_id| {
        projection.turn_items.iter().any(|candidate| {
            matches!(candidate.kind, TurnItemKind::RunInterruptRequest { .. })
                && candidate.run_id.as_deref() == Some(run_id)
        })
    })
}

fn is_superseded_interrupt(
    item: &TurnItem,
    attempts: &[RunAttempt],
    has_interrupt_request: impl Fn(&str) -> bool,
) -> bool {
    let (TurnItemKind::RunInterruptResult { .. }, Some(run_id), Some(node_id)) =
        (&item.kind, item.run_id.as_deref(), item.node_id.as_deref())
    else {
        return false;
    };
    let superseded = attempts.iter().any(|attempt| {
        attempt.run_id == run_id && attempt.root_node_id == node_id && attempt.status == "superseded"
    });
    // Show paired stop-then-steer results; hide legacy plain-steer results without requests.
    superseded && !has_interrupt_request(run_id)
}

/// `activeVisibleTurnItems`: drops local rows whose run state now hides them.
fn refresh_visible_items(projection: &mut ThreadProjection) {
    let statuses: HashMap<&str, &str> =
        projection.runs.iter().map(|run| (run.id.as_str(), run.status.as_str())).collect();
    let mut superseded_roots: HashMap<&str, HashSet<&str>> = HashMap::new();
    for attempt in projection.attempts.iter().filter(|a| a.status == "superseded") {
        superseded_roots.entry(attempt.run_id.as_str()).or_default().insert(attempt.root_node_id.as_str());
    }
    let interrupt_runs: HashSet<&str> = projection
        .turn_items
        .iter()
        .filter(|item| matches!(item.kind, TurnItemKind::RunInterruptRequest { .. }))
        .filter_map(|item| item.run_id.as_deref())
        .collect();
    let visible = |item: &TurnItem| {
        let status = item.run_id.as_deref().and_then(|id| statuses.get(id).copied());
        if status == Some("rolled_back") {
            return false;
        }
        if status == Some("cancelled") && item.is_queued_user_message() {
            return false;
        }
        !(matches!(item.kind, TurnItemKind::RunInterruptResult { .. })
            && item.run_id.as_deref().zip(item.node_id.as_deref()).is_some_and(|(run, node)| {
                superseded_roots.get(run).is_some_and(|roots| roots.contains(node))
                    && !interrupt_runs.contains(run)
            }))
    };
    let before = projection.visible_turn_items.len();
    let keep: Vec<bool> = projection
        .visible_turn_items
        .iter()
        .map(|row| row.visibility != Visibility::Local || visible(&row.item))
        .collect();
    if keep.iter().all(|k| *k) {
        return;
    }
    let mut flags = keep.into_iter();
    projection.visible_turn_items.retain(|_| flags.next().unwrap_or(true));
    if projection.visible_turn_items.len() != before {
        renumber(&mut projection.visible_turn_items);
    }
}

fn oldest_local_ordinal(rows: &[ProjectedTurnItem]) -> Option<u64> {
    rows.iter().filter(|row| row.visibility == Visibility::Local).map(|row| row.item.ordinal).min()
}

fn should_drop_missing_partial_item(
    projection: &ThreadProjection,
    item: &TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if projection.visible_turn_items.iter().any(|row| row.source_item_id == item.id) {
        return false;
    }
    let Some(run_id) = item.run_id.as_deref() else {
        return is_below_partial_window(projection, item, latest_local_turn_ordinal);
    };
    // A queued run keeps the ordinal it got when queued, so a later steer can raise the
    // watermark past it. Items of an unfinished run are new, not unloaded history.
    if projection.runs.iter().any(|run| run.id == run_id && run.is_unfinished()) {
        return false;
    }
    if projection.visible_turn_items.iter().any(|row| {
        row.visibility == Visibility::Local
            && row.item.run_id.as_deref() == Some(run_id)
            && row.item.ordinal <= item.ordinal
    }) {
        return false;
    }
    is_below_partial_window(projection, item, latest_local_turn_ordinal)
}

fn is_below_partial_window(
    projection: &ThreadProjection,
    item: &TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if latest_local_turn_ordinal.is_some_and(|latest| item.ordinal <= latest) {
        return true;
    }
    oldest_local_ordinal(&projection.visible_turn_items).is_some_and(|oldest| item.ordinal < oldest)
}

/// `upsertVisibleTurnItem`.
fn upsert_visible_item(projection: &mut ThreadProjection, item: Arc<TurnItem>, options: ApplyOptions) {
    let index = projection.visible_turn_items.iter().rposition(|row| row.source_item_id == item.id);
    let next = ProjectedTurnItem {
        position: 0,
        visibility: Visibility::Local,
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        item: item.clone(),
    };
    if let Some(i) = index {
        let previous = &projection.visible_turn_items[i];
        if previous.visibility == Visibility::Local && previous.source_thread_id == next.source_thread_id {
            if Arc::ptr_eq(&previous.item, &item) {
                return;
            }
            if previous.item.ordinal == item.ordinal {
                let position = previous.position;
                projection.visible_turn_items[i] = ProjectedTurnItem { position, ..next };
                return;
            }
        }
    }
    if index.is_none()
        && options.partial_timeline
        && should_drop_missing_partial_item(projection, &item, options.latest_local_turn_ordinal)
    {
        return;
    }
    // The item goes before the first later local row, as if its old row were removed first.
    let rows = &projection.visible_turn_items;
    let insertion = rows
        .iter()
        .enumerate()
        .find(|(candidate, row)| {
            Some(*candidate) != index
                && row.visibility == Visibility::Local
                && (row.item.ordinal > item.ordinal
                    || (row.item.ordinal == item.ordinal
                        && locale_compare(&row.item.id, &item.id) == Ordering::Greater))
        })
        .map(|(candidate, _)| candidate)
        .unwrap_or(rows.len());
    let mut updated = Vec::with_capacity(rows.len() + 1);
    for (source, row) in rows.iter().enumerate() {
        if source == insertion {
            updated.push(next.clone());
        }
        if Some(source) != index {
            updated.push(row.clone());
        }
    }
    if insertion == rows.len() {
        updated.push(next);
    }
    renumber(&mut updated);
    projection.visible_turn_items = updated;
}


/// `String.prototype.localeCompare` under the default (root) collation, exact for printable
/// ASCII: punctuation, then digits, then letters case-insensitively, lowercase first on ties.
/// Anything else falls back to code-point order.
pub(crate) fn locale_compare(left: &str, right: &str) -> Ordering {
    const ORDER: &[u8] = b" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";
    fn primary(byte: u8) -> Option<usize> {
        ORDER.iter().position(|candidate| *candidate == byte.to_ascii_lowercase())
    }
    let printable = |s: &str| s.bytes().all(|b| (0x20..0x7f).contains(&b));
    if !printable(left) || !printable(right) {
        return left.cmp(right);
    }
    let primaries = |s: &'_ str| s.bytes().map(primary).collect::<Vec<_>>();
    primaries(left).cmp(&primaries(right)).then_with(|| {
        let tertiary = |s: &'_ str| s.bytes().map(|b| b.is_ascii_uppercase()).collect::<Vec<_>>();
        tertiary(left).cmp(&tertiary(right))
    })
}
