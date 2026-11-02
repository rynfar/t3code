//! One thread's detail, from `orchestration.subscribeThread`.
//! Mirrors `OrchestrationV2ThreadProjection` and `OrchestrationV2ThreadStreamItem`, and the stream
//! handling in `client-runtime/src/state/threads.ts` (sequence dedupe, bounded-window history
//! meta, the partial-timeline watermark). The reducer is in `thread/reduce.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::history::ThreadHistoryMeta;
use crate::model::ModelSelection;
use crate::shell::Lineage;
use crate::turn_item::TurnItem;
use crate::wire;

mod reduce;

pub use reduce::apply_event;
pub(crate) use reduce::is_item_visible;
pub(crate) use reduce::locale_compare;

/// An entity the client stores but does not interpret yet. Upserted by `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: String,
    pub value: Value,
}

impl Serialize for Entity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub active_provider_thread_id: Option<String>,
    /// `native`, or `v1_import` for threads migrated from the first orchestration engine.
    #[serde(default)]
    pub history_origin: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    #[serde(default)]
    pub creation_source: Option<String>,
    /// The last rewind that failed, by the command that asked for it.
    #[serde(default)]
    pub rollback_failure: Option<RollbackFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub ordinal: u64,
    pub provider_instance_id: String,
    pub model_selection: ModelSelection,
    #[serde(default)]
    pub provider_thread_id: Option<String>,
    pub user_message_id: String,
    pub root_node_id: Option<String>,
    pub status: String,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default)]
    pub restart_cancelled_background_work: Option<Value>,
    #[serde(default)]
    pub active_attempt_id: Option<String>,
    #[serde(default)]
    pub queue_held: Option<bool>,
}

impl Run {
    pub fn is_unfinished(&self) -> bool {
        matches!(self.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: String,
    pub run_id: String,
    pub root_node_id: String,
    pub status: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: usize,
    pub visibility: Visibility,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub item: Arc<TurnItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Local,
    Inherited,
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// `thread.deleted`: the reducer replaces the thread; `ThreadState` clears it instead.
    ThreadDeleted(Box<AppThread>),
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
    // Unknown event types only need `type`; known ones are checked by their payload decode.
    #[serde(default)]
    id: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
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
        "thread.deleted" => EventBody::ThreadDeleted(Box::new(from(payload)?)),
        "thread.created" | "thread.archived" | "thread.unarchived"
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
        /// Consumed by `decode_thread_stream_item`; always `None` once decoded through it.
        #[serde(default)]
        turn_items_omit_local_visible: Option<bool>,
    },
    #[serde(rename = "event")]
    Event { sequence: u64, event: Box<DomainEvent> },
    #[serde(other)]
    Unknown,
}

/// Decodes one stream value with the opt-in compactions undone: the snapshot's ID table
/// (`withRecordIdTable`), omitted local turn items, and deduped bodies. Run it off the UI thread.
pub fn decode_thread_stream_item(mut value: Value) -> Result<ThreadStreamItem, serde_json::Error> {
    if let Some(ids) = wire::take_id_table(&mut value)
        && value.get("kind").and_then(Value::as_str) == Some("snapshot")
        && let Some(Value::Object(projection)) = value.get_mut("projection")
    {
        wire::expand_record_ids(projection, &ids);
    }
    let mut item: ThreadStreamItem = serde_json::from_value(value)?;
    if let ThreadStreamItem::Snapshot { projection, turn_items_omit_local_visible, .. } = &mut item {
        wire::restore_snapshot(projection, turn_items_omit_local_visible.take() == Some(true));
    }
    Ok(item)
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
    pub history: ThreadHistoryMeta,
    /// `thread.deleted` arrived: the projection and history are gone until a new snapshot.
    pub deleted: bool,
    /// Bumped whenever the projection is replaced, so a stale history merge is dropped.
    pub(crate) generation: u64,
    /// While a history page merges off the UI thread the projection is checked out; live
    /// events wait here and apply after it, in order, as if the merge were instant.
    pub(crate) merging: Option<Vec<DomainEvent>>,
}

impl ThreadState {
    /// A history page is merging off the UI thread. Views keep showing what they have.
    pub fn is_merging_history(&self) -> bool {
        self.merging.is_some()
    }

    fn replace_projection(&mut self, projection: Option<ThreadProjection>) {
        // Process-wide, so a merge started for one thread can never match another's state.
        static GENERATIONS: AtomicU64 = AtomicU64::new(1);
        self.projection = projection;
        self.generation = GENERATIONS.fetch_add(1, Ordering::Relaxed);
        self.merging = None;
    }

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
                ..
            } => {
                self.history = ThreadHistoryMeta {
                    history_cursor,
                    has_more_history: has_more_history.unwrap_or(false),
                    latest_local_turn_ordinal,
                    ..ThreadHistoryMeta::default()
                };
                self.replace_projection(Some(*projection));
                self.deleted = false;
                self.sequence = Some(snapshot_sequence);
                true
            }
            ThreadStreamItem::Event { sequence, event } => {
                if self.sequence.is_some_and(|current| sequence <= current) {
                    return false;
                }
                self.sequence = Some(sequence);
                self.apply_live_event(*event)
            }
            ThreadStreamItem::Unknown => false,
        }
    }

    /// `applyEvent` in `threads.ts`: deletion clears the thread, a deleted thread ignores
    /// events, and everything else goes through the reducer with the window's options.
    fn apply_live_event(&mut self, event: DomainEvent) -> bool {
        if self.deleted {
            return false;
        }
        if matches!(event.body, EventBody::ThreadDeleted(_)) {
            self.replace_projection(None);
            self.history = ThreadHistoryMeta::default();
            self.deleted = true;
            return true;
        }
        if let Some(pending) = self.merging.as_mut() {
            pending.push(event);
            return false;
        }
        let Some(projection) = self.projection.as_mut() else { return false };
        // Only an incomplete window is partial; `expanded` outlives the last page.
        let options = self.history.apply_options();
        let ordinal = match &event.body {
            EventBody::TurnItemUpserted(item) => Some(item.ordinal),
            _ => None,
        };
        let changed = apply_event(projection, event, options);
        if changed
            && options.partial_timeline
            && let Some(ordinal) = ordinal
            && self.history.latest_local_turn_ordinal.is_none_or(|watermark| ordinal > watermark)
        {
            self.history.latest_local_turn_ordinal = Some(ordinal);
        }
        changed
    }

    /// Applies events that arrived during a history merge, after it.
    pub(crate) fn replay_pending(&mut self, pending: Vec<DomainEvent>) {
        for event in pending {
            self.apply_live_event(event);
        }
    }
}

#[cfg(test)]
mod tests;
