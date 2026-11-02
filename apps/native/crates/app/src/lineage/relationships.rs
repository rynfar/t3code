//! The rows `ThreadRelationshipsPanel` shows for the open thread (Lineage and Side chats in the
//! thread details card), and the actions they offer: merge a fork back, stop a subagent or a
//! workflow (`ThreadRelationshipsControl.tsx`).

use std::collections::HashMap;

use gpui::Context;
use t3_core::lineage::{
    RelationshipKind, STOP_WORKFLOW, Subagent, group_relationships, immediate_relationships, is_parent_relationship, latest_merge_back_run,
    merge_back_command, merge_back_target, order_rows, relationship_graph, relationship_label, relationship_status_label, row_status,
    stop_subagent_command, stop_workflow_request, subagent_display_title, subagents_of, workflow_group,
};
use t3_core::shell::ThreadShell;

use crate::app_state::{AppState, uuid_v4};
use crate::icons::Icon;
use crate::toast::{Toast, ToastKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopAction {
    Subagent { owner: String, subagent_id: String },
    Workflow { thread_id: String, subagent_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageRow {
    pub thread_id: String,
    pub title: String,
    pub icon: Icon,
    pub relationship: &'static str,
    pub status: &'static str,
    pub merge_target: bool,
    pub stop: Option<StopAction>,
}

/// Merging the open fork back: into which thread, and whether a finished turn allows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeBack {
    pub target_title: Option<String>,
    pub ready: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relationships {
    pub lineage: Vec<LineageRow>,
    pub side_chats: Vec<LineageRow>,
    pub merge: Option<MergeBack>,
}

pub fn relationships(state: &AppState, thread: &ThreadShell) -> Relationships {
    let Some(snapshot) = state.shell.snapshot.as_ref() else { return Relationships::default() };
    let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread.id);
    let graph = relationship_graph(snapshot.threads.iter().chain(snapshot.archived_threads.iter()), projection);
    let merge_target = projection.and_then(|projection| merge_back_target(&projection.thread));
    let rows = order_rows(&graph, immediate_relationships(&graph, &thread.id), &thread.id, merge_target.as_deref());
    let agents: Vec<Subagent> = projection.map(subagents_of).unwrap_or_default();
    let by_child: HashMap<&str, &Subagent> = agents.iter().filter_map(|agent| Some((agent.child_thread_id.as_deref()?, agent))).collect();
    let groups = group_relationships(&graph, rows, &thread.id);
    let row = |row: &t3_core::lineage::RelationshipRow| {
        let node = graph.nodes.get(&row.thread_id).and_then(|node| node.thread);
        let parent = is_parent_relationship(&row.edge, &thread.id);
        let subagent = row.edge.kind == RelationshipKind::Subagent;
        let side_chat = !parent && node.is_some_and(|node| node.lineage.side_chat == Some(true));
        let agent = (subagent && !parent).then(|| by_child.get(row.thread_id.as_str()).copied()).flatten();
        let workflow = agent.and_then(workflow_group);
        let status = workflow.as_ref().map(|group| group.status.clone()).or_else(|| row_status(&graph, row));
        let title = workflow
            .as_ref()
            .and_then(|group| group.name.clone())
            .or_else(|| node.map(|node| node.title.clone()))
            .or_else(|| agent.and_then(|agent| agent.title.clone()))
            .unwrap_or_else(|| row.thread_id.clone());
        let stop = agent.and_then(|agent| match &workflow {
            Some(_) if agent.can_stop_workflow() => Some(StopAction::Workflow { thread_id: thread.id.clone(), subagent_id: agent.id.clone() }),
            None if agent.can_stop() => Some(StopAction::Subagent { owner: thread.id.clone(), subagent_id: agent.id.clone() }),
            _ => None,
        });
        LineageRow {
            thread_id: row.thread_id.clone(),
            title: if subagent { subagent_display_title(&title) } else { title },
            icon: if parent {
                Icon::CornerLeftUp
            } else if subagent {
                Icon::Bot
            } else if side_chat {
                Icon::MessagesSquare
            } else {
                Icon::GitFork
            },
            relationship: relationship_label(&row.edge, &thread.id, side_chat),
            status: relationship_status_label(status.as_deref()),
            merge_target: merge_target.as_deref() == Some(row.thread_id.as_str()),
            stop,
        }
    };
    let merge = merge_target.as_ref().map(|target| MergeBack {
        target_title: graph.nodes.get(target).and_then(|node| node.thread).map(|thread| thread.title.clone()),
        ready: projection.is_some_and(|projection| latest_merge_back_run(&projection.runs).is_some()),
    });
    Relationships {
        lineage: groups.related.iter().chain(&groups.active).chain(&groups.previous).map(row).collect(),
        side_chats: groups.side_chats.iter().map(row).collect(),
        merge,
    }
}

impl AppState {
    /// Merges the open fork's latest finished turn into its source, then opens the source.
    pub fn merge_back_selected(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected_thread.clone() else { return };
        let Some(projection) = self.thread.projection.as_ref().filter(|projection| projection.thread.id == source) else { return };
        let (Some(target), Some(run)) = (merge_back_target(&projection.thread), latest_merge_back_run(&projection.runs)) else { return };
        let command = merge_back_command(&uuid_v4(), &source, &target, &run.id);
        self.dispatch_then(command, Some("Could not merge back"), cx, move |state, ok, cx| {
            if ok {
                state.open_related_thread(&target, cx);
            }
        });
    }

    pub fn run_stop_action(&mut self, action: &StopAction, cx: &mut Context<Self>) {
        match action {
            StopAction::Subagent { owner, subagent_id } => self.stop_subagent(owner, subagent_id, cx),
            StopAction::Workflow { thread_id, subagent_id } => {
                let request = self.rpc(STOP_WORKFLOW, stop_workflow_request(thread_id, subagent_id));
                cx.spawn(async move |this, cx| {
                    if request.await.is_err() {
                        let _ = this.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Error, "Could not stop workflow"), cx));
                    }
                })
                .detach();
            }
        }
    }

    /// `interruptThreadTurn` with a `subagentId`: the parent ends the task, and an app-owned task's
    /// child thread is stopped too, which older servers need.
    fn stop_subagent(&mut self, owner: &str, subagent_id: &str, cx: &mut Context<Self>) {
        let Some(projection) = self.thread_state(owner).projection.as_ref() else { return };
        let Some(agent) = subagents_of(projection).into_iter().find(|agent| agent.id == subagent_id) else { return };
        let stop = agent.run_id.as_deref().map(|run_id| stop_subagent_command(&uuid_v4(), owner, run_id, &agent.id));
        match (agent.origin.as_deref(), agent.child_thread_id) {
            (Some("app_owned"), Some(child)) => {
                if let Some(stop) = stop {
                    self.dispatch_then(stop, None, cx, |_, _, _| {});
                }
                self.dispatch_then(t3_core::commands::stop_thread(&uuid_v4(), &child), Some("Could not stop subagent"), cx, |_, _, _| {});
            }
            _ => {
                if let Some(stop) = stop {
                    self.dispatch_then(stop, Some("Could not stop subagent"), cx, |_, _, _| {});
                }
            }
        }
    }
}
