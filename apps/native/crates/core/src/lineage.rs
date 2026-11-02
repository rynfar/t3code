//! How threads relate: parents, forks, subagents, side chats and context transfers, and what a
//! thread can do about them. Ports `client-runtime/src/state/threadRelationships.ts`,
//! `resolveLatestMergeBackRun` from `threadWorkflows.ts`, the row grouping in
//! `web/src/components/chat/ThreadRelationshipsControl.tsx`, and the child inputs a parent shows
//! (`childThreadInputsAtom` and `childThreadInputsTitle` in `client-runtime/src/state/threadShell.ts`).

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::shell::ThreadShell;
use crate::sidebar::thread::{side_chat_request, subagent_request};
use crate::thread::{AppThread, Run, ThreadProjection};
use crate::timeline::parse_iso_ms;

mod subagents;

pub use subagents::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipKind {
    Parent,
    Fork,
    Subagent,
    Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipEdge {
    pub source: String,
    pub target: String,
    pub kind: RelationshipKind,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipNode<'a> {
    pub thread_id: String,
    pub thread: Option<&'a ThreadShell>,
    pub missing: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RelationshipGraph<'a> {
    pub nodes: HashMap<String, RelationshipNode<'a>>,
    pub edges: Vec<RelationshipEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipRow {
    pub thread_id: String,
    pub edge: RelationshipEdge,
}

/// The thread a `forkedFrom` names: only a fork from a run has one.
pub fn forked_from_thread(forked_from: Option<&Value>) -> Option<&str> {
    let source = forked_from?;
    (source.get("type")?.as_str()? == "run").then(|| source.get("threadId")?.as_str()).flatten()
}

fn parent_of<'a>(forked_from: Option<&'a Value>, parent_thread_id: Option<&'a str>) -> Option<&'a str> {
    match forked_from.and_then(|source| source.get("type")).and_then(Value::as_str) {
        Some("run") => forked_from_thread(forked_from),
        _ => parent_thread_id,
    }
}

/// `resolveMergeBackTargetThreadId`: a fork merges back into what it forked from.
pub fn merge_back_target(thread: &AppThread) -> Option<String> {
    if thread.lineage.relationship_to_parent.as_deref() != Some("fork") {
        return None;
    }
    parent_of(thread.forked_from.as_ref(), thread.lineage.parent_thread_id.as_deref()).map(str::to_owned)
}

/// `resolveLatestMergeBackRun`: the newest finished turn, kept through its checkpoint capture
/// (`waiting`), unless a newer turn is still going.
pub fn latest_merge_back_run(runs: &[Run]) -> Option<&Run> {
    let latest = runs.iter().filter(|run| matches!(run.status.as_str(), "waiting" | "completed")).max_by_key(|run| run.ordinal)?;
    let blocked = runs.iter().any(|run| run.ordinal > latest.ordinal && matches!(run.status.as_str(), "preparing" | "starting" | "running"));
    (!blocked).then_some(latest)
}

fn is_work_active(status: &str) -> bool {
    matches!(status, "pending" | "running" | "waiting")
}

/// A thread's edge to its parent, as its lineage records it.
struct Link {
    thread_id: String,
    parent: Option<String>,
    subagent: bool,
    status: Option<String>,
}

struct GraphBuilder<'a> {
    nodes: HashMap<String, RelationshipNode<'a>>,
    edges: Vec<RelationshipEdge>,
    index: HashMap<(String, String, RelationshipKind), usize>,
}

impl<'a> GraphBuilder<'a> {
    fn ensure(&mut self, thread_id: &str) {
        if !self.nodes.contains_key(thread_id) {
            self.nodes.insert(thread_id.to_owned(), RelationshipNode { thread_id: thread_id.to_owned(), thread: None, missing: true });
        }
    }

    /// A later edge for the same pair and kind replaces the earlier one in place.
    fn add(&mut self, mut edge: RelationshipEdge) {
        self.ensure(&edge.source);
        self.ensure(&edge.target);
        if edge.kind == RelationshipKind::Subagent && edge.status.as_deref() == Some("rolled_back") {
            edge.status = Some("interrupted".to_owned());
        }
        let key = (edge.source.clone(), edge.target.clone(), edge.kind);
        match self.index.get(&key) {
            Some(&position) => self.edges[position] = edge,
            None => {
                self.index.insert(key, self.edges.len());
                self.edges.push(edge);
            }
        }
    }
}

/// `deriveThreadRelationshipGraph`. `threads` runs from most to least authoritative: the first
/// copy of a thread wins.
pub fn relationship_graph<'a>(threads: impl IntoIterator<Item = &'a ThreadShell>, projection: Option<&ThreadProjection>) -> RelationshipGraph<'a> {
    let mut builder = GraphBuilder { nodes: HashMap::new(), edges: Vec::new(), index: HashMap::new() };
    let mut ordered: Vec<&'a ThreadShell> = Vec::new();
    for thread in threads {
        if !builder.nodes.contains_key(&thread.id) {
            builder.nodes.insert(thread.id.clone(), RelationshipNode { thread_id: thread.id.clone(), thread: Some(thread), missing: false });
            ordered.push(thread);
        }
    }
    let mut links: Vec<Link> = ordered
        .iter()
        .map(|thread| Link {
            thread_id: thread.id.clone(),
            parent: parent_of(thread.forked_from.as_ref(), thread.lineage.parent_thread_id.as_deref()).map(str::to_owned),
            subagent: thread.lineage.relationship_to_parent.as_deref() == Some("subagent"),
            status: thread.activity_run_status.clone().or_else(|| Some(thread.status.clone())),
        })
        .collect();
    if let Some(selected) = projection.map(|projection| &projection.thread)
        && !ordered.iter().any(|thread| thread.id == selected.id)
    {
        links.push(Link {
            thread_id: selected.id.clone(),
            parent: parent_of(selected.forked_from.as_ref(), selected.lineage.parent_thread_id.as_deref()).map(str::to_owned),
            subagent: selected.lineage.relationship_to_parent.as_deref() == Some("subagent"),
            status: None,
        });
    }
    for link in links {
        let Some(parent) = link.parent else { continue };
        let kind = if link.subagent { RelationshipKind::Subagent } else { RelationshipKind::Fork };
        builder.add(RelationshipEdge { source: parent, target: link.thread_id, kind, status: link.status });
    }
    if let Some(projection) = projection {
        let owner = projection.thread.id.clone();
        for subagent in subagents_of(projection) {
            let Some(child_id) = subagent.child_thread_id.clone() else { continue };
            let child = builder.nodes.get(&child_id).and_then(|node| node.thread);
            let status = child
                .and_then(|child| child.activity_run_status.clone())
                .or_else(|| child.filter(|child| child.latest_run_id.is_some() && !is_work_active(&subagent.status)).map(|child| child.status.clone()))
                .unwrap_or(subagent.status);
            builder.add(RelationshipEdge { source: owner.clone(), target: child_id, kind: RelationshipKind::Subagent, status: Some(status) });
        }
        for transfer in &projection.context_transfers {
            let text = |key: &str| transfer.value.get(key).and_then(Value::as_str).map(str::to_owned);
            let (Some(source), Some(target)) = (text("sourceThreadId"), text("targetThreadId")) else { continue };
            if source == target {
                continue;
            }
            builder.add(RelationshipEdge { source, target, kind: RelationshipKind::Transfer, status: text("status") });
        }
    }
    RelationshipGraph { nodes: builder.nodes, edges: builder.edges }
}

/// `immediateThreadRelationships`: one row per thread directly related to `thread_id`.
pub fn immediate_relationships(graph: &RelationshipGraph, thread_id: &str) -> Vec<RelationshipRow> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for edge in &graph.edges {
        let related = if edge.source == thread_id {
            &edge.target
        } else if edge.target == thread_id {
            &edge.source
        } else {
            continue;
        };
        if seen.insert(related.clone()) {
            rows.push(RelationshipRow { thread_id: related.clone(), edge: edge.clone() });
        }
    }
    rows
}

/// `isParentThreadRelationship`.
pub fn is_parent_relationship(edge: &RelationshipEdge, current: &str) -> bool {
    edge.kind != RelationshipKind::Transfer && edge.target == current
}

/// `threadRelationshipRowStatus`: a child or transfer shows the edge's status; a parent its own.
pub fn row_status(graph: &RelationshipGraph, row: &RelationshipRow) -> Option<String> {
    if row.edge.kind == RelationshipKind::Transfer || row.thread_id == row.edge.target {
        return row.edge.status.clone();
    }
    let thread = graph.nodes.get(&row.thread_id).and_then(|node| node.thread)?;
    thread.activity_run_status.clone().or_else(|| Some(thread.status.clone()))
}

/// `orderWebThreadLineageRows`: the parent, then the merge-back target, then newest first.
pub fn order_rows(graph: &RelationshipGraph, mut rows: Vec<RelationshipRow>, current: &str, merge_target: Option<&str>) -> Vec<RelationshipRow> {
    let rank = |row: &RelationshipRow| {
        if is_parent_relationship(&row.edge, current) {
            0
        } else if Some(row.thread_id.as_str()) == merge_target {
            1
        } else {
            2
        }
    };
    let created = |row: &RelationshipRow| graph.nodes.get(&row.thread_id).and_then(|node| node.thread).and_then(|thread| parse_iso_ms(&thread.created_at));
    rows.sort_by(|left, right| {
        rank(left).cmp(&rank(right)).then_with(|| match (created(left), created(right)) {
            (Some(a), Some(b)) => b.cmp(&a),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, None) => std::cmp::Ordering::Equal,
        })
        .then_with(|| left.thread_id.cmp(&right.thread_id))
    });
    rows
}

/// `relationshipLabel`, with side chats named as such.
pub fn relationship_label(edge: &RelationshipEdge, current: &str, side_chat: bool) -> &'static str {
    let outgoing = edge.source == current;
    match edge.kind {
        _ if side_chat && outgoing => "Side chat",
        RelationshipKind::Transfer => "Context transfer",
        RelationshipKind::Subagent if outgoing => "Subagent",
        RelationshipKind::Subagent => "Parent agent",
        _ if outgoing => "Fork",
        _ => "Parent thread",
    }
}

/// `threadRelationshipStatusLabel`.
pub fn relationship_status_label(status: Option<&str>) -> &'static str {
    match status.unwrap_or("") {
        "preparing" | "starting" => "Starting",
        "running" | "in_progress" => "Running",
        "pending" | "queued" => "Queued",
        "waiting" | "blocked" => "Waiting",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "interrupted" => "Stopped",
        "rolled_back" => "Reverted",
        "resolved_native" => "Resolved (native)",
        "resolved_portable" => "Resolved (portable)",
        "consumed" => "Consumed",
        "superseded" => "Superseded",
        "idle" => "Idle",
        _ => "Unknown",
    }
}

const SETTLED_AGENT_STATUSES: [&str; 6] = ["completed", "failed", "error", "cancelled", "interrupted", "idle"];

/// The relationship panel's rows, grouped as `ThreadRelationshipsPanel` groups them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationshipGroups {
    /// Parents, forks and transfers.
    pub related: Vec<RelationshipRow>,
    pub active: Vec<RelationshipRow>,
    /// "Previous agents", collapsed by default.
    pub previous: Vec<RelationshipRow>,
    pub side_chats: Vec<RelationshipRow>,
}

pub fn group_relationships(graph: &RelationshipGraph, rows: Vec<RelationshipRow>, current: &str) -> RelationshipGroups {
    let mut groups = RelationshipGroups::default();
    for row in rows {
        let parent = is_parent_relationship(&row.edge, current);
        let side_chat = graph.nodes.get(&row.thread_id).and_then(|node| node.thread).is_some_and(|thread| thread.lineage.side_chat == Some(true));
        if !parent && side_chat {
            groups.side_chats.push(row);
        } else if row.edge.kind != RelationshipKind::Subagent || parent {
            groups.related.push(row);
        } else if SETTLED_AGENT_STATUSES.contains(&row.edge.status.as_deref().unwrap_or("")) {
            groups.previous.push(row);
        } else {
            groups.active.push(row);
        }
    }
    groups
}

fn is_child_of(thread: &ThreadShell, parent_id: &str) -> bool {
    let nested = thread.lineage.relationship_to_parent.as_deref() == Some("subagent") || thread.lineage.side_chat == Some(true);
    if nested {
        return thread.lineage.parent_thread_id.as_deref() == Some(parent_id);
    }
    thread.delegated_from.as_ref().and_then(|from| from.get("threadId")).and_then(Value::as_str) == Some(parent_id)
}

/// `childThreadInputsAtom`: subagents and side chats under `parent_id`, at any depth, that wait
/// on the user. Their approvals and questions are hidden with their sidebar rows.
pub fn child_thread_inputs<'a>(threads: &'a [ThreadShell], parent_id: &str) -> Vec<&'a ThreadShell> {
    let mut seen: HashSet<&str> = HashSet::from([parent_id]);
    let mut pending: Vec<&str> = vec![parent_id];
    let mut inputs = Vec::new();
    let mut index = 0;
    while let Some(&parent) = pending.get(index) {
        index += 1;
        for child in threads.iter().filter(|thread| is_child_of(thread, parent)) {
            if !seen.insert(child.id.as_str()) {
                continue;
            }
            pending.push(child.id.as_str());
            if subagent_request(child).is_some() || side_chat_request(child).is_some() {
                inputs.push(child);
            }
        }
    }
    inputs
}

/// Whether the child asks for an approval rather than an answer (`hasPendingApprovals`).
pub fn asks_approval(thread: &ThreadShell) -> bool {
    thread.pending_runtime_request.as_ref().is_some_and(|request| request.kind != "user_input")
}

/// `childThreadInputsTitle`.
pub fn child_thread_inputs_title(children: &[&ThreadShell]) -> String {
    let approvals = children.iter().filter(|child| asks_approval(child)).count();
    let need = if approvals == 0 {
        "input"
    } else if approvals == children.len() {
        "approval"
    } else {
        "you"
    };
    let side_chats = children.iter().filter(|child| child.lineage.side_chat == Some(true)).count();
    if children.len() == 1 {
        let noun = if side_chats == 1 { "Side chat" } else { "Subagent" };
        return format!("{noun} needs {need}");
    }
    let noun = if side_chats == children.len() {
        "side chat"
    } else if side_chats == 0 {
        "subagent"
    } else {
        "thread"
    };
    format!("{} {noun}s need {need}", children.len())
}

/// The `waitingLabel` of `childInputNowItem`.
pub fn child_inputs_waiting_label(children: &[&ThreadShell]) -> &'static str {
    if children.iter().all(|child| child.lineage.side_chat == Some(true)) { "Side chat waiting" } else { "Subagent waiting" }
}

/// `thread.merge_back` (`mergeThreadBack`).
pub fn merge_back_command(command_id: &str, source_thread_id: &str, target_thread_id: &str, run_id: &str) -> Value {
    json!({
        "type": "thread.merge_back",
        "commandId": command_id,
        "createdBy": "user",
        "creationSource": crate::commands::CREATION_SOURCE,
        "sourceThreadId": source_thread_id,
        "targetThreadId": target_thread_id,
        "sourcePoint": { "type": "run", "runId": run_id },
    })
}

/// `subagent.stop`: ends one subagent of `thread_id`'s run.
pub fn stop_subagent_command(command_id: &str, thread_id: &str, run_id: &str, subagent_id: &str) -> Value {
    json!({ "type": "subagent.stop", "commandId": command_id, "threadId": thread_id, "runId": run_id, "subagentId": subagent_id })
}

pub const STOP_WORKFLOW: &str = "orchestration.stopWorkflow";

pub fn stop_workflow_request(thread_id: &str, subagent_id: &str) -> Value {
    json!({ "threadId": thread_id, "subagentId": subagent_id })
}

#[cfg(test)]
mod tests;
