//! The rows `ThreadRelationshipsPanel` shows for the open thread (Lineage and Side chats in the
//! thread details card), and the actions they offer: merge a fork back, stop a subagent or a
//! workflow (`ThreadRelationshipsControl.tsx`).

use std::collections::{HashMap, HashSet};

use gpui::Context;
use t3_core::lineage::{
    RelationshipGraph, RelationshipKind, RelationshipRow, STOP_WORKFLOW, Subagent, WorkflowGroup, group_relationships, immediate_relationships,
    is_parent_relationship, keep_relationship_row, latest_merge_back_run, merge_back_command, merge_back_target, order_rows,
    relationship_graph, relationship_label, relationship_status_label, row_status, stop_subagent_command, stop_workflow_request,
    subagent_display_title, subagents_of, unlisted_title_ids, workflow_group,
};
use t3_core::shell::ThreadShell;
use t3_core::thread::ThreadProjection;

use super::titles::ThreadTitlesFeed;
use crate::app_state::{AppState, uuid_v4};
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
    pub merge: Option<MergeBack>,
    pub running_count: usize,
    pub running_side_chats: usize,
    pub show_heading: bool,
}

impl Relationships {
    pub fn lineage_len(&self) -> usize {
        self.related.len() + self.active.len() + self.previous.len()
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
    let show_heading = running_count > 0 || !related.is_empty() || !active.is_empty();
    Relationships { related, active, previous, side_chats, merge, running_count, running_side_chats, show_heading }
}

/// The runtime title `projectedSubagentsToRuntime` gives a subagent: its title, else its prompt.
fn runtime_title(agent: &Subagent) -> String {
    agent.title.clone().unwrap_or_else(|| {
        if agent.prompt.chars().count() > 80 { format!("{}...", agent.prompt.chars().take(77).collect::<String>()) } else { agent.prompt.clone() }
    })
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
        });
        assert!(ro_fixture.take_commands().is_empty(), "read-only connection must not dispatch merge or stop");
    }
}
