//! One thread's detail, from `orchestration.subscribeThread`.
//! Mirrors `OrchestrationV2ThreadProjection`, `OrchestrationV2ThreadStreamItem`, and the reducer in
//! `client-runtime/src/state/orchestrationV2Projection.ts` with the visibility rules from
//! `shared/src/orchestrationV2Timeline.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::model::ModelSelection;
use crate::shell::Lineage;
use crate::turn_item::{TurnItem, TurnItemKind};

/// An entity the client stores but does not interpret yet. Upserted by `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: String,
    pub value: Value,
}

impl<'de> Deserialize<'de> for Entity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::missing_field("id"))?
            .to_owned();
        Ok(Entity { id, value })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub lineage: Lineage,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub ordinal: u64,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    pub user_message_id: String,
    pub root_node_id: Option<String>,
    pub status: String,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default)]
    pub restart_cancelled_background_work: Option<Value>,
}

impl Run {
    pub fn is_unfinished(&self) -> bool {
        matches!(self.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    pub run_id: String,
    pub root_node_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: usize,
    pub visibility: Visibility,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub item: Arc<TurnItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Local,
    Inherited,
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<Entity>,
    pub subagents: Vec<Entity>,
    pub provider_sessions: Vec<Entity>,
    pub provider_threads: Vec<Entity>,
    pub provider_turns: Vec<Entity>,
    pub runtime_requests: Vec<Entity>,
    pub messages: Vec<Entity>,
    pub plans: Vec<Entity>,
    #[serde(deserialize_with = "known_turn_items")]
    pub turn_items: Vec<Arc<TurnItem>>,
    pub checkpoint_scopes: Vec<Entity>,
    pub checkpoints: Vec<Entity>,
    pub context_handoffs: Vec<Entity>,
    pub context_transfers: Vec<Entity>,
    #[serde(deserialize_with = "known_projected_items")]
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: String,
}

fn known_turn_items<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Arc<TurnItem>>, D::Error> {
    let items = Vec::<TurnItem>::deserialize(d)?;
    Ok(items.into_iter().filter(TurnItem::is_known).map(Arc::new).collect())
}

fn known_projected_items<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ProjectedTurnItem>, D::Error> {
    let rows = Vec::<ProjectedTurnItem>::deserialize(d)?;
    Ok(rows.into_iter().filter(|row| row.item.is_known()).collect())
}

/// A domain event from the thread stream. Payloads the reducer ignores are dropped at decode.
#[derive(Debug, Clone)]
pub struct DomainEvent {
    pub id: String,
    pub thread_id: String,
    pub occurred_at: String,
    pub body: EventBody,
}

#[derive(Debug, Clone)]
pub enum EventBody {
    /// Every `thread.*` event that carries the whole thread and counts as activity.
    ThreadReplaced(Box<AppThread>),
    /// `thread.visited`, `thread.tags-updated`, `thread.marked-unread`: not activity.
    ThreadTouched(Box<AppThread>),
    ThreadPullRequestLinkUpdated,
    RunUpserted(Box<Run>),
    RunBackgroundWorkCancelled { run_id: String, restart_cancelled_background_work: Option<Value> },
    AttemptUpserted(RunAttempt),
    NodeUpserted(Entity),
    SubagentUpserted(Entity),
    ProviderSessionUpserted(Entity),
    ProviderSessionDetached { provider_session_id: String },
    ProviderThreadUpserted(Entity),
    ProviderTurnUpserted(Entity),
    RuntimeRequestUpserted(Entity),
    MessageUpserted(Entity),
    PlanUpserted(Entity),
    TurnItemUpserted(Arc<TurnItem>),
    CheckpointScopeUpserted(Entity),
    CheckpointCaptured(Entity),
    CheckpointRollbackRequested,
    ContextHandoffUpserted(Entity),
    ContextTransferUpserted(Entity),
    Unknown(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    id: String,
    thread_id: String,
    occurred_at: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: Value,
}

impl<'de> Deserialize<'de> for DomainEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawEvent::deserialize(deserializer)?;
        let body = decode_body(&raw.kind, raw.payload).map_err(serde::de::Error::custom)?;
        Ok(DomainEvent { id: raw.id, thread_id: raw.thread_id, occurred_at: raw.occurred_at, body })
    }
}

fn decode_body(kind: &str, payload: Value) -> Result<EventBody, serde_json::Error> {
    use serde_json::from_value as from;
    Ok(match kind {
        "thread.created" | "thread.archived" | "thread.unarchived" | "thread.deleted"
        | "thread.settled" | "thread.unsettled" | "thread.snoozed" | "thread.unsnoozed"
        | "thread.auto-settle-set" | "thread.pinned" | "thread.unpinned" | "thread.pin-reordered"
        | "thread.active-reordered" | "thread.metadata-updated" | "thread.pull-request-synced"
        | "thread.runtime-mode-updated" | "thread.interaction-mode-updated"
        | "thread.model-selection-updated" | "thread.provider-switched"
        | "thread.guest-share-updated" => EventBody::ThreadReplaced(Box::new(from(payload)?)),
        "thread.visited" | "thread.tags-updated" | "thread.marked-unread" => {
            EventBody::ThreadTouched(Box::new(from(payload)?))
        }
        "thread.pull-request-link-updated" => EventBody::ThreadPullRequestLinkUpdated,
        "run.created" | "run.updated" => EventBody::RunUpserted(Box::new(from(payload)?)),
        "run.background-work-cancelled" => EventBody::RunBackgroundWorkCancelled {
            run_id: payload.get("runId").and_then(Value::as_str).unwrap_or_default().to_owned(),
            restart_cancelled_background_work: payload.get("restartCancelledBackgroundWork").cloned(),
        },
        "run-attempt.created" | "run-attempt.updated" => EventBody::AttemptUpserted(from(payload)?),
        "node.updated" => EventBody::NodeUpserted(from(payload)?),
        "subagent.updated" => EventBody::SubagentUpserted(from(payload)?),
        "provider-session.attached" | "provider-session.updated" => {
            EventBody::ProviderSessionUpserted(from(payload)?)
        }
        "provider-session.detached" => EventBody::ProviderSessionDetached {
            provider_session_id: payload
                .get("providerSessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "provider-thread.updated" => EventBody::ProviderThreadUpserted(from(payload)?),
        "provider-turn.updated" => EventBody::ProviderTurnUpserted(from(payload)?),
        "runtime-request.updated" => EventBody::RuntimeRequestUpserted(from(payload)?),
        "message.updated" => EventBody::MessageUpserted(from(payload)?),
        "plan.updated" => EventBody::PlanUpserted(from(payload)?),
        "turn-item.updated" => {
            let item: TurnItem = from(payload)?;
            if item.is_known() {
                EventBody::TurnItemUpserted(Arc::new(item))
            } else {
                EventBody::Unknown(kind.to_owned())
            }
        }
        "checkpoint-scope.created" => EventBody::CheckpointScopeUpserted(from(payload)?),
        "checkpoint.captured" => EventBody::CheckpointCaptured(from(payload)?),
        "checkpoint.rollback-requested" => EventBody::CheckpointRollbackRequested,
        "context-handoff.updated" => EventBody::ContextHandoffUpserted(from(payload)?),
        "context-transfer.created" | "context-transfer.updated" => {
            EventBody::ContextTransferUpserted(from(payload)?)
        }
        other => EventBody::Unknown(other.to_owned()),
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum ThreadStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot", rename_all = "camelCase")]
    Snapshot {
        snapshot_sequence: u64,
        projection: Box<ThreadProjection>,
        #[serde(default)]
        history_cursor: Option<String>,
        #[serde(default)]
        has_more_history: Option<bool>,
        #[serde(default)]
        latest_local_turn_ordinal: Option<u64>,
    },
    #[serde(rename = "event")]
    Event { sequence: u64, event: Box<DomainEvent> },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOptions {
    pub partial_timeline: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

/// Client-side thread detail state for one subscription.
#[derive(Debug, Clone, Default)]
pub struct ThreadState {
    pub projection: Option<ThreadProjection>,
    pub sequence: Option<u64>,
    pub synchronized: bool,
    pub has_more_history: bool,
    pub options: ApplyOptions,
}

impl ThreadState {
    /// Returns true when visible state changed.
    pub fn apply(&mut self, item: ThreadStreamItem) -> bool {
        match item {
            ThreadStreamItem::Synchronized => !std::mem::replace(&mut self.synchronized, true),
            ThreadStreamItem::Snapshot {
                snapshot_sequence,
                projection,
                history_cursor,
                has_more_history,
                latest_local_turn_ordinal,
            } => {
                let bounded = history_cursor.is_some() || has_more_history.is_some();
                self.has_more_history = has_more_history.unwrap_or(false);
                self.options = ApplyOptions {
                    partial_timeline: bounded,
                    latest_local_turn_ordinal,
                };
                self.projection = Some(*projection);
                self.sequence = Some(snapshot_sequence);
                true
            }
            ThreadStreamItem::Event { sequence, event } => {
                if self.sequence.is_some_and(|current| sequence <= current) {
                    return false;
                }
                self.sequence = Some(sequence);
                match self.projection.as_mut() {
                    Some(projection) => apply_event(projection, *event, self.options),
                    None => false,
                }
            }
            ThreadStreamItem::Unknown => false,
        }
    }
}

/// Applies one domain event in place. Returns true when the projection changed.
pub fn apply_event(projection: &mut ThreadProjection, event: DomainEvent, options: ApplyOptions) -> bool {
    if event.thread_id != projection.thread.id {
        return false;
    }
    let touches_activity = !matches!(
        event.body,
        EventBody::ThreadTouched(_) | EventBody::Unknown(_)
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
        EventBody::SubagentUpserted(entity) => upsert_entity(&mut projection.subagents, entity),
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
        EventBody::TurnItemUpserted(item) => return apply_turn_item(projection, item, options),
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

fn apply_turn_item(projection: &mut ThreadProjection, item: Arc<TurnItem>, options: ApplyOptions) -> bool {
    // Streamed updates land on the newest items, so search from the end.
    let index = projection.turn_items.iter().rposition(|candidate| candidate.id == item.id);
    if options.partial_timeline
        && index.is_none()
        && should_drop_missing_partial_item(projection, &item, options.latest_local_turn_ordinal)
    {
        return false;
    }
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
fn is_item_visible(projection: &ThreadProjection, item: &TurnItem) -> bool {
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
    // TS compares ids with localeCompare; ids are ASCII, so byte order only differs on case.
    let rows = &projection.visible_turn_items;
    let insertion = rows
        .iter()
        .enumerate()
        .find(|(candidate, row)| {
            Some(*candidate) != index
                && row.visibility == Visibility::Local
                && (row.item.ordinal > item.ordinal
                    || (row.item.ordinal == item.ordinal && row.item.id.as_str() > item.id.as_str()))
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

#[cfg(test)]
mod tests;
