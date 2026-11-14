//! The rows `ThreadRelationshipsPanel` shows for the open thread (Lineage and Side chats in the
//! thread details card), and the actions they offer: merge a fork back, stop a subagent or a
//! workflow (`ThreadRelationshipsControl.tsx`).

use std::collections::{HashMap, HashSet};

use gpui::Context;
use t3_core::auth::ORCHESTRATION_READ_SCOPE;
use t3_core::lineage::{
    AgentTiming, GET_THREAD_TITLES, RelationshipGraph, RelationshipKind, RelationshipRow, STOP_WORKFLOW, Subagent, WorkflowGroup, decode_thread_titles,
    group_relationships, immediate_relationships, is_parent_relationship, keep_relationship_row, latest_merge_back_run, merge_back_command,
    merge_back_target, order_rows, relationship_graph, relationship_label, relationship_status_label, row_status, stop_subagent_command,
    stop_workflow_request, subagent_display_title, subagent_prompt_title, subagents_of, thread_titles_request, unlisted_title_ids, workflow_group,
};
use t3_core::shell::ThreadShell;
use t3_core::thread::ThreadProjection;
use t3_core::workflows::{can_detach_provider_session, detach_session_commands};

use super::titles::ThreadTitlesFeed;
use crate::app_state::{AppState, ScopedRouteAction, uuid_v4};
use crate::icons::Icon;
use crate::toast::{Toast, ToastKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopAction {
    Subagent { owner: String, subagent_id: String },
    Workflow { thread_id: String, subagent_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineageRow {
    pub thread_id: String,
    pub title: String,
    pub icon: Icon,
    pub relationship: &'static str,
    pub status: &'static str,
    pub failed: bool,
    pub is_running: bool,
    pub merge_target: bool,
    pub stop: Option<StopAction>,
    pub workflow: Option<WorkflowGroup>,
    pub timing: Option<AgentTiming>,
}

/// A parent thread on a linked environment that delegated this thread (`DelegatedFromRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedFromRow {
    pub environment_id: String,
    pub thread_id: String,
    pub title: String,
    pub env_label: Option<String>,
    pub connected: bool,
}

/// A subagent task delegated to a linked environment (`RemoteSubagentRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSubagentRow {
    pub id: String,
    pub environment_id: String,
    pub thread_id: String,
    pub env_label: String,
    pub title: String,
    pub status: &'static str,
    pub failed: bool,
    pub connected: bool,
}

/// Merging the open fork back: into which thread, and whether a finished turn allows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeBack {
    pub target_title: Option<String>,
    pub ready: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Relationships {
    pub related: Vec<LineageRow>,
    pub active: Vec<LineageRow>,
    pub previous: Vec<LineageRow>,
    pub side_chats: Vec<LineageRow>,
    pub delegated_from: Option<DelegatedFromRow>,
    pub remote_subagents: Vec<RemoteSubagentRow>,
    pub merge: Option<MergeBack>,
    pub running_count: usize,
    pub running_side_chats: usize,
    pub can_detach: bool,
    pub show_heading: bool,
}

impl Relationships {
    pub fn has_lineage(&self) -> bool {
        !self.related.is_empty()
            || !self.active.is_empty()
            || !self.previous.is_empty()
            || self.running_count > 0
            || !self.remote_subagents.is_empty()
            || self.delegated_from.is_some()
            || (self.can_detach && !self.side_chats.is_empty())
    }

    pub fn lineage_len(&self) -> usize {
        let rows = self.related.len()
            + self.active.len()
            + self.previous.len()
            + usize::from(self.delegated_from.is_some())
            + self.remote_subagents.len();
        if rows > 0 { rows } else { usize::from(self.has_lineage()) }
    }
}

struct Prepared<'a> {
    projection: Option<&'a ThreadProjection>,
    graph: RelationshipGraph<'a>,
    rows: Vec<RelationshipRow>,
    agents: Vec<Subagent>,
    merge_target: Option<String>,
    unlisted_ids: Vec<String>,
}

fn prepare<'a>(state: &'a AppState, thread: &ThreadShell) -> Option<Prepared<'a>> {
    let snapshot = state.shell.snapshot.as_ref()?;
    let projection = state.thread.projection.as_ref().filter(|projection| projection.thread.id == thread.id);
    // Only live shells count as listed (`relationshipShells`); omitted/archived shells still feed
    // graph structure and fallback metadata, while `getThreadTitles` fetches fresh titles/existence.
    let listed_set: HashSet<&str> = snapshot.threads.iter().map(|shell| shell.id.as_str()).collect();
    let family = state.omitted.mounts.extras(&state.shell);
    let listed: Vec<&'a ThreadShell> = snapshot.threads.iter().chain(family).chain(snapshot.archived_threads.iter()).collect();
    let graph = relationship_graph(listed, projection);
    let merge_target = projection.and_then(|projection| merge_back_target(&projection.thread));
    let hiding_anywhere = state.hiding_thread_tags() || !state.sidebar.hidden_thread_ids.is_empty();
    let rows: Vec<RelationshipRow> = order_rows(&graph, immediate_relationships(&graph, &thread.id), &thread.id, merge_target.as_deref())
        .into_iter()
        .filter(|row| {
            !state.sidebar.hidden_thread_ids.contains(&row.thread_id)
                && !(hiding_anywhere
                    && graph.nodes.get(&row.thread_id).is_some_and(|node| node.missing)
                    && (row.edge.kind != RelationshipKind::Subagent || is_parent_relationship(&row.edge, &thread.id)))
        })
        .collect();
    let agents: Vec<Subagent> = projection.map(subagents_of).unwrap_or_default();
    let subagents_with_titles: HashSet<&str> =
        agents.iter().filter(|agent| agent.title.is_some()).filter_map(|agent| agent.child_thread_id.as_deref()).collect();
    let unlisted_ids = unlisted_title_ids(&rows, &listed_set, &subagents_with_titles);
    Some(Prepared { projection, graph, rows, agents, merge_target, unlisted_ids })
}

pub(super) fn unlisted_ids(state: &AppState, thread: &ThreadShell) -> Vec<String> {
    prepare(state, thread).map(|prepared| prepared.unlisted_ids).unwrap_or_default()
}

pub fn relationships_with_titles(state: &AppState, thread: &ThreadShell, titles: Option<&ThreadTitlesFeed>) -> Relationships {
    let Some(prepared) = prepare(state, thread) else { return Relationships::default() };
    let unlisted_set: HashSet<&str> = prepared.unlisted_ids.iter().map(String::as_str).collect();
    let resolved = titles.and_then(|feed| feed.resolved_for(state, &prepared.unlisted_ids));
    let by_child: HashMap<&str, &Subagent> = prepared.agents.iter().filter_map(|agent| Some((agent.child_thread_id.as_deref()?, agent))).collect();
    let visible_rows: Vec<RelationshipRow> = prepared
        .rows
        .into_iter()
        .filter(|row| keep_relationship_row(row, &unlisted_set, resolved))
        .map(|mut row| {
            if row.edge.kind == RelationshipKind::Subagent
                && !is_parent_relationship(&row.edge, &thread.id)
                && let Some(group) = by_child.get(row.thread_id.as_str()).copied().and_then(workflow_group)
            {
                row.edge.status = Some(group.status);
            }
            row
        })
        .collect();
    let can_operate = state.can_operate_threads();
    let graph = &prepared.graph;
    let merge_target = prepared.merge_target.as_deref().filter(|target| visible_rows.iter().any(|row| row.thread_id == *target));
    let groups = group_relationships(graph, visible_rows, &thread.id);
    let row = |row: &RelationshipRow| {
        let node = graph.nodes.get(&row.thread_id).and_then(|node| node.thread);
        let parent = is_parent_relationship(&row.edge, &thread.id);
        let subagent = row.edge.kind == RelationshipKind::Subagent;
        let side_chat = !parent && node.is_some_and(|node| node.lineage.side_chat == Some(true));
        let agent = (subagent && !parent).then(|| by_child.get(row.thread_id.as_str()).copied()).flatten();
        let workflow = agent.and_then(workflow_group);
        let status = workflow.as_ref().map(|group| group.status.clone()).or_else(|| row_status(graph, row));
        let failed = matches!(status.as_deref(), Some("failed" | "error"));
        let is_running = status.as_deref() == Some("running");
        let title = workflow
            .as_ref()
            .and_then(|group| group.name.clone())
            .or_else(|| resolved.and_then(|map| map.get(&row.thread_id).cloned()))
            .or_else(|| node.map(|node| node.title.clone()))
            .or_else(|| agent.map(runtime_title))
            .unwrap_or_else(|| row.thread_id.clone());
        let stop = can_operate
            .then(|| {
                agent.and_then(|agent| match &workflow {
                    Some(_) if agent.can_stop_workflow() => Some(StopAction::Workflow { thread_id: thread.id.clone(), subagent_id: agent.id.clone() }),
                    None if agent.can_stop(node) => Some(StopAction::Subagent { owner: thread.id.clone(), subagent_id: agent.id.clone() }),
                    _ => None,
                })
            })
            .flatten();
        let timing = agent.and_then(|agent| {
            if workflow.is_some() {
                AgentTiming::from_subagent(agent)
            } else {
                AgentTiming::from_subagent(&agent.current(node))
            }
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
            failed,
            is_running,
            merge_target: merge_target == Some(row.thread_id.as_str()),
            stop,
            workflow,
            timing,
        }
    };
    let merge = merge_target.map(|target| MergeBack {
        target_title: resolved
            .and_then(|map| map.get(target).cloned())
            .or_else(|| graph.nodes.get(target).and_then(|node| node.thread).map(|thread| thread.title.clone())),
        ready: can_operate && prepared.projection.is_some_and(|projection| latest_merge_back_run(&projection.runs).is_some()),
    });
    let related: Vec<LineageRow> = groups.related.iter().map(row).collect();
    let active: Vec<LineageRow> = groups.active.iter().map(row).collect();
    let previous: Vec<LineageRow> = groups.previous.iter().map(row).collect();
    let side_chats: Vec<LineageRow> = groups.side_chats.iter().map(row).collect();
    let running_count = prepared.agents.iter().filter(|agent| agent.child_thread_id.is_none() && agent.status == "running").count()
        + active.iter().filter(|row| row.is_running).count();
    let running_side_chats = side_chats.iter().filter(|row| row.is_running).count();
    let can_detach = can_operate && prepared.projection.is_some_and(can_detach_provider_session);
    let delegated_from = thread.delegated_from.as_ref().and_then(|value| {
        let environment_id = value.get("environmentId")?.as_str()?.to_owned();
        let parent_id = value.get("threadId")?.as_str()?.to_owned();
        let fallback_title = value.get("title")?.as_str()?.to_owned();
        let (connected, env_label, parent_title) = resolve_linked_target(state, &environment_id, &parent_id);
        Some(DelegatedFromRow {
            environment_id,
            thread_id: parent_id,
            title: parent_title.unwrap_or(fallback_title),
            env_label: env_label.or_else(|| thread.link_origin.as_ref().map(|origin| origin.label.clone())),
            connected,
        })
    });
    let remote_subagents = prepared
        .agents
        .iter()
        .filter_map(|agent| {
            let remote = agent.remote_child.as_ref()?;
            let environment_id = remote.get("environmentId")?.as_str()?.to_owned();
            let remote_thread_id = remote.get("threadId")?.as_str()?.to_owned();
            let env_label = remote.get("label")?.as_str()?.to_owned();
            let (connected, _, _) = resolve_linked_target(state, &environment_id, &remote_thread_id);
            let title = subagent_display_title(agent.title.as_deref().unwrap_or("Subagent"));
            let failed = agent.status == "failed";
            Some(RemoteSubagentRow {
                id: agent.id.clone(),
                environment_id,
                thread_id: remote_thread_id,
                env_label,
                title,
                status: relationship_status_label(Some(&agent.status)),
                failed,
                connected,
            })
        })
        .collect();
    let show_heading = running_count > 0 || can_detach || !related.is_empty() || !active.is_empty() || delegated_from.is_some();
    Relationships {
        related,
        active,
        previous,
        side_chats,
        delegated_from,
        remote_subagents,
        merge,
        running_count,
        running_side_chats,
        can_detach,
        show_heading,
    }
}

fn resolve_linked_target(state: &AppState, canonical_env_id: &str, thread_id: &str) -> (bool, Option<String>, Option<String>) {
    if !state.fleet.environments.is_empty() {
        let entry = state.fleet.environments.iter().find(|entry| entry.summary.owner.canonical_id.as_deref() == Some(canonical_env_id));
        let connected = entry.is_some_and(|entry| state.scoped_ready(&entry.summary.owner, ORCHESTRATION_READ_SCOPE));
        let label = entry.and_then(|entry| {
            entry
                .config
                .config
                .as_ref()
                .map(|config| config.environment.label.clone())
                .or_else(|| state.environments.get(&entry.summary.owner.saved_id).map(|saved| saved.label.clone()))
        });
        let title = entry
            .and_then(|entry| entry.summary.threads.iter().find(|t| t.thread.id == thread_id))
            .map(|t| t.thread.title.clone());
        return (connected, label, title);
    }
    let self_matches = state.server_config.config.as_ref().is_some_and(|config| config.environment.environment_id == canonical_env_id);
    if !self_matches {
        return (false, None, None);
    }
    let label = state.server_config.config.as_ref().map(|config| config.environment.label.clone());
    let title = state.shell.snapshot.as_ref().and_then(|s| s.threads.iter().find(|t| t.id == thread_id)).map(|t| t.title.clone());
    (state.client_handle().is_some(), label, title)
}

/// The runtime title `projectedSubagentsToRuntime` gives a subagent: its title, else its prompt.
fn runtime_title(agent: &Subagent) -> String {
    agent.title.clone().unwrap_or_else(|| subagent_prompt_title(&agent.prompt))
}

impl AppState {
    /// Merges the open fork's latest finished turn into its source, then opens the source.
    pub fn merge_back_selected(&mut self, cx: &mut Context<Self>) {
        if !self.can_operate_threads() {
            return;
        }
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

    pub fn detach_provider_session(&mut self, cx: &mut Context<Self>) {
        if !self.can_operate_threads() {
            return;
        }
        let Some(source) = self.selected_thread.clone() else { return };
        let Some(projection) = self.thread.projection.as_ref().filter(|projection| projection.thread.id == source) else { return };
        if !can_detach_provider_session(projection) {
            return;
        }
        for command in detach_session_commands(&uuid_v4(), projection) {
            self.dispatch_then(command, Some("Could not disconnect agent session"), cx, |_, _, _| {});
        }
    }

    pub fn open_linked_thread(&mut self, environment_id: &str, thread_id: &str, cx: &mut Context<Self>) {
        if let Some(owner) = self
            .fleet
            .environments
            .iter()
            .find(|entry| entry.summary.owner.canonical_id.as_deref() == Some(environment_id))
            .map(|entry| entry.summary.owner.clone())
            .filter(|owner| self.scoped_ready(owner, ORCHESTRATION_READ_SCOPE))
        {
            self.navigate_scoped(owner, ScopedRouteAction::Thread(thread_id.to_owned()), cx);
            return;
        }
        if self.fleet.environments.is_empty()
            && self.server_config.config.as_ref().is_some_and(|config| config.environment.environment_id == environment_id)
        {
            self.open_related_thread(thread_id, cx);
        }
    }

    /// Checks `orchestration.getThreadTitles` on the parent thread's environment before opening it
    /// so a deleted remote parent keeps the child thread open with an info toast.
    pub fn open_delegated_parent(&mut self, environment_id: &str, thread_id: &str, cx: &mut Context<Self>) {
        if self.opening_delegated_parent {
            return;
        }
        let fleet_owner = self
            .fleet
            .environments
            .iter()
            .find(|entry| entry.summary.owner.canonical_id.as_deref() == Some(environment_id))
            .map(|entry| entry.summary.owner.clone())
            .filter(|owner| self.scoped_ready(owner, ORCHESTRATION_READ_SCOPE));
        let local_match = self.fleet.environments.is_empty()
            && self.server_config.config.as_ref().is_some_and(|config| config.environment.environment_id == environment_id);
        let destination = fleet_owner.as_ref().map(|owner| self.connections.destination(owner, ORCHESTRATION_READ_SCOPE));
        let local_rpc = local_match.then(|| self.rpc(GET_THREAD_TITLES, thread_titles_request(&[thread_id.to_owned()])));
        if destination.is_none() && local_rpc.is_none() {
            return;
        }
        self.opening_delegated_parent = true;
        let origin_thread = self.selected_thread.clone();
        let origin_connection = self.connection_id();
        let thread_id = thread_id.to_owned();
        let payload = thread_titles_request(std::slice::from_ref(&thread_id));
        cx.spawn(async move |this, cx| {
            let outcome: Result<HashMap<String, String>, String> = async {
                let raw = match destination {
                    Some(dest) => dest.await.map_err(|e| format!("{e:?}"))?.client.request(GET_THREAD_TITLES, payload).await.map_err(|e| e.to_string())?,
                    None => local_rpc.expect("local_rpc checked").await.map_err(|e| e.to_string())?,
                };
                decode_thread_titles(raw)
            }
            .await;
            let _ = this.update(cx, |state, cx| {
                state.opening_delegated_parent = false;
                if state.selected_thread != origin_thread || state.connection_id() != origin_connection {
                    return;
                }
                match outcome {
                    Err(_) => state.show_toast(Toast::new(ToastKind::Error, "Could not open the parent thread"), cx),
                    Ok(titles) if !titles.contains_key(&thread_id) => {
                        state.show_toast(Toast::new(ToastKind::Info, "The parent thread was deleted"), cx);
                    }
                    Ok(_) => match fleet_owner {
                        Some(owner) => state.navigate_scoped(owner, ScopedRouteAction::Thread(thread_id), cx),
                        None => state.open_related_thread(&thread_id, cx),
                    },
                }
            });
        })
        .detach();
    }

    pub fn run_stop_action(&mut self, action: &StopAction, cx: &mut Context<Self>) {
        if !self.can_operate_threads() {
            return;
        }
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
                if let Some(stop_child) = self.stop_command(&child) {
                    self.dispatch_then(stop_child, Some("Could not stop subagent"), cx, |_, _, _| {});
                }
            }
            _ => {
                if let Some(stop) = stop {
                    self.dispatch_then(stop, Some("Could not stop subagent"), cx, |_, _, _| {});
                }
            }
        }
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use gpui::{Context, Entity, IntoElement, Render, Window};
    use t3_core::thread::ThreadStreamItem;

    use super::*;

    struct TitlesHarness {
        state: Entity<AppState>,
        titles: ThreadTitlesFeed,
        latest: Relationships,
        settled_tx: tokio::sync::mpsc::UnboundedSender<()>,
    }

    impl TitlesHarness {
        fn refresh(&mut self, cx: &mut Context<Self>) {
            self.titles.sync(
                &self.state,
                true,
                |this| &mut this.titles,
                |this, cx| {
                    let app = this.state.read(cx);
                    if let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) {
                        this.latest = relationships_with_titles(app, thread, Some(&this.titles));
                    }
                    let _ = this.settled_tx.send(());
                    cx.notify();
                },
                cx,
            );
            let app = self.state.read(cx);
            if let Some(thread) = app.selected_thread.as_deref().and_then(|id| app.thread_shell(id)) {
                self.latest = relationships_with_titles(app, thread, Some(&self.titles));
            }
        }
    }

    impl Render for TitlesHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui::div()
        }
    }

    #[gpui::test]
    async fn lineage_prunes_deleted_forks_and_refreshes_unlisted_titles_plus_gates_operate(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (rw_client, mut fixture) = t3_client::supervisor::ClientFixture::handle(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": [t3_core::auth::ORCHESTRATION_READ_SCOPE, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE]
            }))
            .unwrap(),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            let thread = shell.snapshot.as_mut().unwrap().threads.iter_mut().find(|t| t.id == "menu-thread").unwrap();
            thread.lineage.parent_thread_id = Some("fork-deleted".into());
            thread.lineage.relationship_to_parent = Some("fork".into());
            // Archived thread with stale title in snapshot.archived_threads must STILL be fetched
            // via getThreadTitles, and the resolved live title must win over the archived copy.
            let mut archived: ThreadShell = thread.clone();
            archived.id = "fork-archived".into();
            archived.title = "Archived Stale Title".into();
            archived.lineage.parent_thread_id = None;
            archived.lineage.relationship_to_parent = None;
            archived.archived_at = Some("2026-04-01T00:00:00.000Z".into());
            shell.snapshot.as_mut().unwrap().archived_threads.push(archived);
            state.install_active_environment(rw_client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            state.select_thread("menu-thread", cx);
            let snapshot: ThreadStreamItem = serde_json::from_value(serde_json::json!({
                "kind": "snapshot",
                "snapshotSequence": 1,
                "hasMoreHistory": false,
                "historyCursor": null,
                "projection": {
                    "thread": {
                        "id": "menu-thread", "projectId": "checkout-a", "title": "Main",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default",
                        "lineage": {"rootThreadId":"fork-deleted","parentThreadId":"fork-deleted","relationshipToParent":"fork"},
                        "forkedFrom": {"type":"run","threadId":"fork-deleted","runId":"r0"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "completed",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": null,
                        "completedAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "attempts": [], "nodes": [],
                    "subagents": [
                        {
                            "id": "sub-done", "threadId": "menu-thread", "childThreadId": "child-done",
                            "origin": "app_owned", "prompt": "done agent", "title": "Done Agent",
                            "status": "completed", "updatedAt": "2026-04-01T00:00:02.000Z"
                        },
                        {
                            "id": "sub-live", "threadId": "menu-thread", "childThreadId": "child-live",
                            "origin": "app_owned", "prompt": "live agent", "title": "Live Agent",
                            "status": "running", "updatedAt": "2026-04-01T00:00:03.000Z"
                        }
                    ],
                    "providerSessions": [], "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [],
                    "contextTransfers": [{
                        "id": "tx-1", "sourceThreadId": "fork-archived", "targetThreadId": "menu-thread", "status": "completed"
                    }],
                    "visibleTurnItems": [],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }))
            .unwrap();
            state.thread.apply(snapshot);
        });
        while !fixture.take_commands().is_empty() {}

        let (settled_tx, mut settled_rx) = tokio::sync::mpsc::unbounded_channel();
        let (view, vcx) = cx.add_window_view(|_, cx| {
            let mut h = TitlesHarness { state: state.clone(), titles: ThreadTitlesFeed::new(), latest: Relationships::default(), settled_tx };
            h.refresh(cx);
            h
        });

        // Unsettled lookup preserves unlisted IDs and separates active vs previous groups.
        vcx.update(|_, cx| {
            view.read_with(cx, |h, _| {
                let ids: Vec<&str> = h.latest.related.iter().map(|r| r.thread_id.as_str()).collect();
                assert_eq!(ids, vec!["fork-deleted", "fork-archived"]);
                assert_eq!(h.latest.active.iter().map(|r| r.thread_id.as_str()).collect::<Vec<_>>(), vec!["child-live"]);
                assert_eq!(h.latest.previous.iter().map(|r| r.thread_id.as_str()).collect::<Vec<_>>(), vec!["child-done"]);
                assert_eq!(h.latest.running_count, 1);
                assert!(h.latest.show_heading);
                assert!(h.latest.merge.as_ref().is_some_and(|m| m.ready));
            });
        });

        // First getThreadTitles fails -> feed must recover (not stay locked on active_key).
        let (method_err, payload_err, reply_err) = loop {
            let req = fixture.next_request().await;
            if req.0 == t3_core::lineage::GET_THREAD_TITLES {
                break req;
            }
            let _ = req.2.send(Ok(serde_json::json!({})));
        };
        assert_eq!(method_err, t3_core::lineage::GET_THREAD_TITLES);
        assert_eq!(payload_err["threadIds"], serde_json::json!(["fork-archived", "fork-deleted"]));
        reply_err.send(Err(t3_client::session::RpcError::Closed)).unwrap();

        // Advancing event/retry clears failed_key and fires retry.
        vcx.update(|_, cx| {
            view.update(cx, |h, cx| {
                h.titles.allow_retry();
                h.refresh(cx);
            });
        });

        let (method, payload, reply) = loop {
            let req = fixture.next_request().await;
            if req.0 == t3_core::lineage::GET_THREAD_TITLES {
                break req;
            }
            let _ = req.2.send(Ok(serde_json::json!({})));
        };
        assert_eq!(method, t3_core::lineage::GET_THREAD_TITLES);
        assert_eq!(payload["threadIds"], serde_json::json!(["fork-archived", "fork-deleted"]));
        reply.send(Ok(serde_json::json!({ "fork-archived": "Archived Fork Fresh Title" }))).unwrap();

        settled_rx.recv().await.unwrap();
        vcx.update(|_, cx| {
            view.read_with(cx, |h, _| {
                assert_eq!(h.latest.related.len(), 1);
                assert_eq!(h.latest.related[0].thread_id, "fork-archived");
                assert_eq!(h.latest.related[0].title, "Archived Fork Fresh Title");
                assert!(h.latest.merge.is_none(), "deleted merge target clears merge button");
            });

            // Hidden thread IDs filter related rows.
            state.update(cx, |state, _| {
                state.sidebar.hidden_thread_ids.insert("fork-archived".into());
            });
            view.update(cx, |h, cx| h.refresh(cx));
            view.read_with(cx, |h, _| {
                assert!(h.latest.related.is_empty(), "hidden thread id must be filtered from lineage");
            });
            state.update(cx, |state, _| {
                state.sidebar.hidden_thread_ids.remove("fork-archived");
                state.shell.removed_at.insert("fork-archived".into(), 2);
            });
            view.update(cx, |h, cx| h.refresh(cx));
        });
        let (method2, payload2, reply2) = loop {
            let req = fixture.next_request().await;
            if req.0 == t3_core::lineage::GET_THREAD_TITLES {
                break req;
            }
            let _ = req.2.send(Ok(serde_json::json!({})));
        };
        assert_eq!(method2, t3_core::lineage::GET_THREAD_TITLES);
        assert_eq!(payload2["threadIds"], serde_json::json!(["fork-archived", "fork-deleted"]));
        reply2.send(Ok(serde_json::json!({}))).unwrap();

        settled_rx.recv().await.unwrap();
        view.read_with(cx, |h, _| {
            assert!(h.latest.related.is_empty(), "removed archived fork disappears after re-lookup");
        });

        // Read-only permissions block workflow stop / merge mutations.
        let (ro_client, mut ro_fixture) = t3_client::supervisor::ClientFixture::handle(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": [t3_core::auth::ORCHESTRATION_READ_SCOPE]
            }))
            .unwrap(),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            state.install_active_environment(ro_client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
        });
        let _ = ro_fixture.take_commands();
        state.update(cx, |state, cx| {
            state.merge_back_selected(cx);
            state.run_stop_action(&StopAction::Workflow { thread_id: "menu-thread".into(), subagent_id: "sub-1".into() }, cx);
            state.detach_provider_session(cx);
        });
        assert!(ro_fixture.take_commands().is_empty(), "read-only connection must not dispatch merge, stop, or detach");

        // Unlinked running subagents, remote subagents, delegatedFrom, and live session detach keep lineage visible.
        state.update(cx, |state, cx| {
            state.install_active_environment(
                t3_client::supervisor::ClientFixture::handle(
                    serde_json::from_value(serde_json::json!({
                        "authenticated": true,
                        "permissions": [t3_core::auth::ORCHESTRATION_READ_SCOPE, t3_core::auth::ORCHESTRATION_OPERATE_SCOPE]
                    }))
                    .unwrap(),
                )
                .0,
                state.shell.clone(),
                state.server_config.clone(),
                t3_client::supervisor::ConnectionStatus::Connected,
                cx,
            );
            let snap = state.shell.snapshot.as_mut().unwrap();
            snap.archived_threads.clear();
            snap.threads.retain(|t| t.id == "menu-thread");
            for t in &mut snap.threads {
                t.lineage = t3_core::shell::Lineage { parent_thread_id: None, relationship_to_parent: None, root_thread_id: "menu-thread".into(), side_chat: None };
                t.forked_from = None;
                t.delegated_from = Some(serde_json::json!({"environmentId": "remote-env", "threadId": "remote-parent", "title": "Remote Parent"}));
            }
            let snapshot: ThreadStreamItem = serde_json::from_value(serde_json::json!({
                "kind": "snapshot",
                "snapshotSequence": 2,
                "hasMoreHistory": false,
                "historyCursor": null,
                "projection": {
                    "thread": {
                        "id": "menu-thread", "projectId": "checkout-a", "title": "Main",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "runtimeMode": "auto", "interactionMode": "default",
                        "activeProviderThreadId": "pt-1",
                        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "menu-thread"},
                        "forkedFrom": null
                    },
                    "runs": [], "attempts": [], "nodes": [],
                    "subagents": [
                        {
                            "id": "sub-unlinked", "threadId": "menu-thread", "childThreadId": null,
                            "origin": "provider_native", "prompt": "unlinked", "title": "Unlinked",
                            "status": "running", "updatedAt": "2026-04-01T00:00:03.000Z"
                        },
                        {
                            "id": "sub-remote", "threadId": "menu-thread", "childThreadId": null,
                            "remoteChild": {"environmentId": "remote-env", "threadId": "rc-1", "label": "Remote Mac"},
                            "origin": "app_owned", "prompt": "remote", "title": "Subagent: Remote Check",
                            "status": "running", "updatedAt": "2026-04-01T00:00:03.000Z"
                        }
                    ],
                    "providerSessions": [{"id": "ps-1", "status": "ready"}],
                    "providerThreads": [{"id": "pt-1", "appThreadId": "menu-thread", "providerSessionId": "ps-1"}],
                    "providerTurns": [], "runtimeRequests": [], "messages": [], "plans": [],
                    "turnItems": [], "checkpointScopes": [], "checkpoints": [],
                    "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }))
            .unwrap();
            state.thread.apply(snapshot);
            let thread = state.thread_shell("menu-thread").unwrap().clone();
            let rel = relationships_with_titles(state, &thread, None);
            assert!(rel.related.is_empty() && rel.active.is_empty() && rel.previous.is_empty());
            assert_eq!(rel.running_count, 2);
            assert!(rel.can_detach);
            assert!(rel.show_heading);
            assert!(rel.has_lineage());
            assert!(rel.lineage_len() >= 2);
            assert_eq!(rel.delegated_from.as_ref().map(|d| (d.title.as_str(), d.connected)), Some(("Remote Parent", false)));
            assert_eq!(rel.remote_subagents.len(), 1);
            assert_eq!(rel.remote_subagents[0].title, "Remote Check");
            assert_eq!(rel.remote_subagents[0].env_label, "Remote Mac");

            // A completed non-workflow subagent whose child thread resumes uses `agent.current(node)` for timing.
            let mut resumed_child_json = serde_json::to_value(state.thread_shell("menu-thread").unwrap()).unwrap();
            resumed_child_json["id"] = serde_json::json!("child-resumed");
            resumed_child_json["title"] = serde_json::json!("Resumed Child");
            resumed_child_json["status"] = serde_json::json!("running");
            resumed_child_json["activityRunStatus"] = serde_json::json!("running");
            resumed_child_json["activityRunStartedAt"] = serde_json::json!("2026-04-01T00:04:00.000Z");
            resumed_child_json["latestRunRequestedAt"] = serde_json::json!("2026-04-01T00:04:00.000Z");
            resumed_child_json["delegatedFrom"] = serde_json::Value::Null;
            resumed_child_json["lineage"] = serde_json::json!({
                "parentThreadId": "menu-thread",
                "relationshipToParent": "subagent",
                "rootThreadId": "menu-thread"
            });
            let resumed_child: ThreadShell = serde_json::from_value(resumed_child_json).unwrap();
            state.shell.snapshot.as_mut().unwrap().threads.push(resumed_child);
            let mut resumed_proj = serde_json::to_value(state.thread.projection.as_ref().unwrap()).unwrap();
            resumed_proj["subagents"] = serde_json::json!([
                {
                    "id": "sub-resumed",
                    "threadId": "menu-thread",
                    "childThreadId": "child-resumed",
                    "origin": "app_owned",
                    "prompt": "resumed",
                    "title": "Subagent: Resumed Child",
                    "status": "completed",
                    "startedAt": "2026-04-01T00:00:01.000Z",
                    "completedAt": "2026-04-01T00:00:10.000Z",
                    "updatedAt": "2026-04-01T00:00:10.000Z"
                }
            ]);
            let resumed_snapshot: ThreadStreamItem = serde_json::from_value(serde_json::json!({
                "kind": "snapshot",
                "snapshotSequence": 3,
                "hasMoreHistory": false,
                "historyCursor": null,
                "projection": resumed_proj
            }))
            .unwrap();
            state.thread.apply(resumed_snapshot);
            let thread = state.thread_shell("menu-thread").unwrap().clone();
            let rel = relationships_with_titles(state, &thread, None);
            assert_eq!(rel.active.len(), 1);
            assert_eq!(
                rel.active[0].timing,
                Some(AgentTiming {
                    active: true,
                    started_ms: t3_core::timeline::parse_iso_ms("2026-04-01T00:04:00.000Z"),
                    completed_ms: None,
                })
            );
        });
    }

    #[gpui::test]
    async fn delegated_parent_checks_thread_titles_and_shows_deleted_toast(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (notice_tx, mut notice_rx) = tokio::sync::mpsc::unbounded_channel();
        let ((state, mut fixture), _notice_sub) = cx.update(|cx| {
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            let sub = cx.subscribe(&installed.0, move |_, event, _| {
                if matches!(event, crate::app_state::AppEvent::Notice) {
                    let _ = notice_tx.send(());
                }
            });
            (installed, sub)
        });
        state.update(cx, |state, cx| {
            state.select_thread("menu-thread", cx);
            state.open_delegated_parent("fixture-env", "deleted-parent", cx);
            assert!(state.opening_delegated_parent);
            // Concurrent duplicate click is ignored while in-flight.
            state.open_delegated_parent("fixture-env", "deleted-parent", cx);
        });
        let (params, reply) = loop {
            let (method, params, reply) = fixture.next_request().await;
            if method == GET_THREAD_TITLES {
                break (params, reply);
            }
            reply.send(Ok(serde_json::json!({}))).unwrap();
        };
        assert_eq!(params["threadIds"], serde_json::json!(["deleted-parent"]));
        reply.send(Ok(serde_json::json!({}))).unwrap();
        notice_rx.recv().await.unwrap();
        state.read_with(cx, |state, _| {
            assert!(!state.opening_delegated_parent);
            assert_eq!(state.selected_thread.as_deref(), Some("menu-thread"));
            assert_eq!(
                state.toasts.latest().map(|t| (t.kind, t.title.as_ref())),
                Some((ToastKind::Info, "The parent thread was deleted"))
            );
        });

        state.update(cx, |state, cx| state.open_delegated_parent("fixture-env", "parent-2", cx));
        let (_, reply) = loop {
            let (method, params, reply) = fixture.next_request().await;
            if method == GET_THREAD_TITLES {
                break (params, reply);
            }
            reply.send(Ok(serde_json::json!({}))).unwrap();
        };
        reply.send(Err(t3_client::session::RpcError::Closed)).unwrap();
        notice_rx.recv().await.unwrap();
        state.read_with(cx, |state, _| {
            assert_eq!(
                state.toasts.latest().map(|t| (t.kind, t.title.as_ref())),
                Some((ToastKind::Error, "Could not open the parent thread"))
            );
        });
        drop(_notice_sub);
    }
}
