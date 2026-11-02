use serde_json::{Value, json};

use super::*;

const T: &str = "2026-11-01T00:00:00.000Z";

fn merge(mut value: Value, extra: Value) -> Value {
    if let (Value::Object(base), Value::Object(extra)) = (&mut value, extra) {
        base.extend(extra);
    }
    value
}

fn shell(id: &str, status: &str, extra: Value) -> ThreadShell {
    let value = json!({
        "id": id, "projectId": "p1", "title": format!("Title {id}"), "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "auto",
        "interactionMode": "default", "branch": null, "worktreePath": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "latestRunId": "run-1", "activeRunId": null, "status": status, "pendingRuntimeRequest": null,
        "latestVisibleMessage": null, "latestUserMessageAt": null, "hasActionableProposedPlan": false,
        "itemCount": 1, "visibleItemCount": 1, "createdAt": T, "updatedAt": T, "archivedAt": null,
        "settledOverride": null, "settledAt": null, "deletedAt": null
    });
    serde_json::from_value(merge(value, extra)).expect("shell")
}

fn child(id: &str, parent: &str, relationship: &str, status: &str, extra: Value) -> ThreadShell {
    let lineage = json!({"lineage": {"parentThreadId": parent, "relationshipToParent": relationship, "rootThreadId": parent}});
    shell(id, status, merge(lineage, extra))
}

fn run(id: &str, ordinal: u64, status: &str, started: bool) -> Value {
    json!({
        "id": id, "ordinal": ordinal, "providerInstanceId": "codex", "modelSelection": {"instanceId": "codex", "model": "gpt"},
        "userMessageId": "m0", "rootNodeId": null, "status": status, "requestedAt": T,
        "startedAt": if started { Value::from(T) } else { Value::Null }, "completedAt": null
    })
}

fn runs(values: Vec<Value>) -> Vec<Run> {
    values.into_iter().map(|value| serde_json::from_value(value).expect("run")).collect()
}

fn subagent(id: &str, run_id: Option<&str>, status: &str, updated: &str, extra: Value) -> Value {
    merge(
        json!({
            "id": id, "threadId": "t1", "runId": run_id, "parentNodeId": "n0", "origin": "provider_native",
            "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null, "childThreadId": null,
            "nativeTaskRef": null, "prompt": format!("Do {id}"), "title": null, "model": null, "status": status,
            "result": null, "startedAt": updated, "completedAt": null, "updatedAt": updated
        }),
        extra,
    )
}

fn decode_agents(values: Vec<Value>) -> Vec<Subagent> {
    values.into_iter().map(|value| serde_json::from_value(value).expect("subagent")).collect()
}

fn projection(thread: Value, subagents: Vec<Value>, transfers: Vec<Value>, run_values: Vec<Value>) -> ThreadProjection {
    let thread = merge(
        json!({
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "full-access",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "createdAt": T, "updatedAt": T, "archivedAt": null
        }),
        thread,
    );
    serde_json::from_value(json!({
        "thread": thread, "runs": run_values, "attempts": [], "nodes": [], "subagents": subagents, "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [], "plans": [], "turnItems": [],
        "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": transfers,
        "visibleTurnItems": [], "updatedAt": T
    }))
    .expect("projection")
}

#[test]
fn a_missing_parent_never_takes_its_childs_running_status() {
    let threads = [child("child", "missing-parent", "subagent", "running", json!({}))];
    let graph = relationship_graph(&threads, None);
    let rows = immediate_relationships(&graph, "child");
    assert_eq!(rows.len(), 1);
    assert!(graph.nodes["missing-parent"].missing);
    assert_eq!(row_status(&graph, &rows[0]), None);
}

#[test]
fn an_opened_thread_omitted_from_the_shell_keeps_its_parent_edge_and_merge_target() {
    for relationship in ["fork", "subagent"] {
        let forked_from = if relationship == "fork" { json!({"type": "run", "threadId": "parent", "runId": "r"}) } else { Value::Null };
        let projection = projection(
            json!({"id": "child", "forkedFrom": forked_from, "lineage": {"parentThreadId": "parent", "relationshipToParent": relationship, "rootThreadId": "parent"}}),
            vec![],
            vec![],
            vec![],
        );
        let graph = relationship_graph(std::iter::empty(), Some(&projection));
        let rows = immediate_relationships(&graph, "child");
        let kind = if relationship == "fork" { RelationshipKind::Fork } else { RelationshipKind::Subagent };
        assert_eq!(rows, [RelationshipRow { thread_id: "parent".into(), edge: RelationshipEdge { source: "parent".into(), target: "child".into(), kind, status: None } }]);
        assert_eq!(merge_back_target(&projection.thread).as_deref(), (relationship == "fork").then_some("parent"));
    }
}

#[test]
fn a_fork_from_a_run_merges_back_into_the_recorded_source_over_its_lineage_parent() {
    let projection = projection(
        json!({"forkedFrom": {"type": "run", "threadId": "source", "runId": "r"}, "lineage": {"parentThreadId": "lineage-parent", "relationshipToParent": "fork", "rootThreadId": "x"}}),
        vec![],
        vec![],
        vec![],
    );
    assert_eq!(merge_back_target(&projection.thread).as_deref(), Some("source"));
    let node_fork = projection.thread.clone();
    let mut node_fork = node_fork;
    node_fork.forked_from = Some(json!({"type": "node", "nodeId": "n"}));
    assert_eq!(merge_back_target(&node_fork).as_deref(), Some("lineage-parent"));
}

#[test]
fn subagent_and_transfer_edges_join_the_shell_edges_and_rolled_back_reads_interrupted() {
    let threads = [shell("t1", "completed", json!({})), child("kid", "t1", "subagent", "completed", json!({"archivedAt": T}))];
    let projection = projection(
        json!({}),
        vec![
            subagent("a1", Some("r1"), "completed", T, json!({"childThreadId": "kid"})),
            subagent("a2", Some("r1"), "rolled_back", T, json!({"childThreadId": "ghost"})),
        ],
        vec![
            json!({"id": "x1", "sourceThreadId": "kid", "targetThreadId": "elsewhere", "status": "completed"}),
            json!({"id": "x2", "sourceThreadId": "t1", "targetThreadId": "t1", "status": "completed"}),
        ],
        vec![],
    );
    let graph = relationship_graph(&threads, Some(&projection));
    assert!(graph.nodes["kid"].thread.is_some_and(|thread| thread.archived_at.is_some()));
    assert!(graph.nodes["elsewhere"].missing);
    let edges: Vec<(&str, &str, RelationshipKind, Option<&str>)> =
        graph.edges.iter().map(|edge| (edge.source.as_str(), edge.target.as_str(), edge.kind, edge.status.as_deref())).collect();
    assert_eq!(
        edges,
        [
            ("t1", "kid", RelationshipKind::Subagent, Some("completed")),
            ("t1", "ghost", RelationshipKind::Subagent, Some("interrupted")),
            ("kid", "elsewhere", RelationshipKind::Transfer, Some("completed")),
        ]
    );
}

#[test]
fn the_first_copy_of_a_thread_wins_over_a_stale_archived_one() {
    let live = shell("t2", "running", json!({"title": "Live"}));
    let stale = shell("t2", "completed", json!({"title": "Stale"}));
    let threads = [live, stale];
    let graph = relationship_graph(&threads, None);
    assert_eq!(graph.nodes["t2"].thread.map(|thread| thread.title.as_str()), Some("Live"));
}

#[test]
fn rows_pin_the_parent_then_the_merge_target_then_go_newest_first() {
    let threads = [
        shell("current", "completed", json!({"lineage": {"parentThreadId": "parent", "relationshipToParent": "fork", "rootThreadId": "parent"}})),
        shell("parent", "completed", json!({"createdAt": "2026-10-01T00:00:00.000Z"})),
        child("old", "current", "subagent", "completed", json!({"createdAt": "2026-11-01T00:00:00.000Z"})),
        child("new", "current", "subagent", "running", json!({"createdAt": "2026-11-02T00:00:00.000Z"})),
        child("undated", "current", "subagent", "running", json!({"createdAt": "not a date"})),
    ];
    let graph = relationship_graph(&threads, None);
    let ordered = order_rows(&graph, immediate_relationships(&graph, "current"), "current", Some("old"));
    let ids: Vec<&str> = ordered.iter().map(|row| row.thread_id.as_str()).collect();
    assert_eq!(ids, ["parent", "old", "new", "undated"]);
}

#[test]
fn groups_split_side_chats_live_agents_and_previous_agents() {
    let threads = [
        shell("current", "completed", json!({})),
        child("live", "current", "subagent", "running", json!({})),
        child("done", "current", "subagent", "completed", json!({})),
        child("side", "current", "fork", "completed", json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "fork", "rootThreadId": "current", "sideChat": true}})),
        child("fork", "current", "fork", "completed", json!({})),
    ];
    let graph = relationship_graph(&threads, None);
    let groups = group_relationships(&graph, immediate_relationships(&graph, "current"), "current");
    let ids = |rows: &[RelationshipRow]| rows.iter().map(|row| row.thread_id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&groups.related), ["fork"]);
    assert_eq!(ids(&groups.active), ["live"]);
    assert_eq!(ids(&groups.previous), ["done"]);
    assert_eq!(ids(&groups.side_chats), ["side"]);
}

#[test]
fn merge_back_keeps_a_turn_capturing_its_checkpoint_unless_a_newer_turn_runs() {
    let finished = runs(vec![run("r1", 1, "completed", true), run("r2", 2, "waiting", true), run("r3", 3, "queued", false)]);
    assert_eq!(latest_merge_back_run(&finished).map(|run| run.id.as_str()), Some("r2"));
    let blocked = runs(vec![run("r1", 1, "completed", true), run("r2", 2, "running", true)]);
    assert_eq!(latest_merge_back_run(&blocked), None);
    assert_eq!(latest_merge_back_run(&runs(vec![run("r1", 1, "failed", true)])), None);
}

#[test]
fn the_turn_roster_follows_the_active_run_then_the_latest_started_one() {
    let at = |minute: u32| format!("2026-11-01T00:{minute:02}:00.000Z");
    let agents = decode_agents(vec![
        subagent("b", Some("r2"), "running", &at(3), json!({})),
        subagent("a", Some("r2"), "completed", &at(2), json!({})),
        subagent("old", Some("r1"), "completed", &at(9), json!({})),
        subagent("background", Some("r1"), "running", &at(1), json!({})),
    ]);
    let active = turn_subagents(&runs(vec![run("r1", 1, "completed", true), run("r2", 2, "running", true)]), &agents).expect("roster");
    let ids: Vec<&str> = active.subagents.iter().map(|agent| agent.id.as_str()).collect();
    assert_eq!(ids, ["background", "a", "b"]);
    assert_eq!((active.live_count, active.settled_count, active.turn_active, active.latest_turn), (2, 1, true, true));
    assert_eq!(subagent_pill(Some(&active)).map(|pill| pill.label), Some("2/3".into()));

    let settled = decode_agents(vec![subagent("x", Some("r1"), "completed", &at(1), json!({}))]);
    let later = runs(vec![run("r1", 1, "completed", true), run("r2", 2, "completed", true)]);
    let turn = turn_subagents(&later, &settled).expect("roster");
    assert!(!turn.latest_turn);
    assert_eq!(subagent_pill(Some(&turn)), None, "a later turn replaced the finished agents");
    let rolled_back = runs(vec![run("r1", 1, "completed", true), run("r2", 2, "rolled_back", true)]);
    let turn = turn_subagents(&rolled_back, &settled).expect("roster");
    assert_eq!(subagent_pill(Some(&turn)).map(|pill| pill.accessibility_label), Some("1 agent done".into()));
    assert_eq!(turn_subagents(&runs(vec![run("r1", 1, "rolled_back", true)]), &settled), None);
}

#[test]
fn workflow_members_follow_an_ended_coordinator_and_phases_merge_declared_titles() {
    let coordinator = decode_agents(vec![subagent(
        "w",
        None,
        "cancelled",
        T,
        json!({"workflow": {"name": "Review", "phases": [{"index": 0, "title": "Plan"}], "agents": [
            {"index": 1, "label": "B", "state": "running", "phaseIndex": 0},
            {"index": 0, "label": "A", "state": "completed", "phaseIndex": 0},
            {"index": 2, "label": "C", "state": "queued", "phaseIndex": 1, "phaseTitle": "Build"},
            {"index": 3, "label": "D", "state": "queued"}
        ]}}),
    )])
    .remove(0);
    let group = workflow_group(&coordinator).expect("group");
    let phases: Vec<String> = group
        .phases
        .iter()
        .map(|phase| {
            let members: Vec<String> = phase.members.iter().map(|m| format!("{}={}", m.label, m.status)).collect();
            format!("{} {} {:?} [{}]", phase.index, phase.title, phase.state, members.join(" "))
        })
        .collect();
    assert_eq!(phases, ["0 Plan Done [A=completed B=cancelled]", "1 Build Done [C=cancelled]"]);
    assert_eq!(group.unphased.iter().map(|m| m.status.as_str()).collect::<Vec<_>>(), ["cancelled"]);
}

#[test]
fn display_titles_drop_the_prefix_and_name_root_paths() {
    assert_eq!(subagent_display_title("Subagent: Explore the repo"), "Explore the repo");
    assert_eq!(subagent_display_title("/root/agents/code_reviewer/"), "Code Reviewer");
    assert_eq!(subagent_display_title("/rootless"), "/rootless");
    assert_eq!(subagent_display_title(" Subagent: x"), " Subagent: x");
}

#[test]
fn provider_subagent_status_reads_the_runless_root_turn() {
    let mut native = projection(
        json!({"creationSource": "provider", "lineage": {"parentThreadId": "p", "relationshipToParent": "subagent", "rootThreadId": "p"}}),
        vec![],
        vec![],
        vec![],
    );
    native.nodes = vec![
        serde_json::from_value(json!({"id": "n1", "kind": "root_turn", "runId": "r1", "status": "completed"})).expect("node"),
        serde_json::from_value(json!({"id": "n2", "kind": "root_turn", "runId": null, "status": "running", "startedAt": "2026-11-01T00:00:00.000Z", "completedAt": null}))
            .expect("node"),
    ];
    let status = provider_subagent_status(&native).expect("status");
    let start = parse_iso_ms(T).expect("time");
    assert_eq!(format_provider_subagent_status(Some(&status), start + 65_400), "Working 1m 5s");
    let done = ProviderSubagentStatus { status: "completed".into(), started_at: Some(T.into()), completed_at: Some("2026-11-01T00:00:42.000Z".into()) };
    assert_eq!(format_provider_subagent_status(Some(&done), 0), "Completed in 42s");
    let failed = ProviderSubagentStatus { status: "failed".into(), started_at: None, completed_at: None };
    assert_eq!(format_provider_subagent_status(Some(&failed), 0), "Failed");
    assert_eq!(format_provider_subagent_status(None, 0), "Starting");
    let ordinary = projection(json!({}), vec![], vec![], vec![]);
    assert_eq!(provider_subagent_status(&ordinary), None);
}

#[test]
fn native_child_work_decodes_and_labels_steps() {
    let children = decode_native_child_work(json!({"children": [{
        "conversationId": "c1", "role": "", "prompt": "Look", "truncated": true,
        "steps": [{"index": 0, "type": "TOOL_CALL", "status": "RUNNING", "text": null, "toolCalls": [{"name": "grep", "input": "x"}]},
                  {"index": 1, "type": "PLANNER_RESPONSE", "status": "DONE", "text": "ok", "toolCalls": []}]
    }, {"conversationId": "c2", "role": "Reviewer", "prompt": "p", "steps": null, "truncated": false}]}))
    .expect("decode");
    let steps = children[0].steps.as_ref().expect("steps");
    assert_eq!(steps[0].heading(), "Tool call · Running");
    assert_eq!(steps[1].heading(), "Planner response");
    assert_eq!(children[1].steps, None);
    assert!(decode_native_child_work(json!({"nope": 1})).is_err());
}

#[test]
fn commands_carry_the_fields_the_server_reads() {
    assert_eq!(merge_back_command("c", "s", "t", "r")["sourcePoint"], json!({"type": "run", "runId": "r"}));
    assert_eq!(stop_subagent_command("c", "t", "r", "a"), json!({"type": "subagent.stop", "commandId": "c", "threadId": "t", "runId": "r", "subagentId": "a"}));
    assert_eq!(stop_workflow_request("t", "a"), json!({"threadId": "t", "subagentId": "a"}));
}

#[test]
fn effort_reads_the_reported_selection_first_and_variant_falls_back_to_its_default() {
    let model: crate::server_config::ProviderModel = serde_json::from_value(json!({
        "slug": "gpt", "name": "GPT", "isCustom": false,
        "capabilities": {"optionDescriptors": [
            {"id": "fastMode", "label": "Fast", "type": "boolean"},
            {"id": "reasoningEffort", "label": "Reasoning", "type": "select", "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}]}
        ]}
    }))
    .expect("model");
    let selection = |options: Value| -> crate::model::ModelSelection { serde_json::from_value(json!({"instanceId": "codex", "model": "gpt", "options": options})).expect("selection") };
    let chosen = selection(json!([{"id": "reasoningEffort", "value": "low"}]));
    assert_eq!(effort_label(&chosen, Some(&model), None).as_deref(), Some("Low"));
    let reported = selection(json!([{"id": "reasoningEffort", "value": "high"}]));
    assert_eq!(effort_label(&chosen, Some(&model), Some(&reported)).as_deref(), Some("High"));
    assert_eq!(effort_label(&selection(json!([])), Some(&model), None), None, "an unstated effort is not shown");
    assert_eq!(effort_label(&chosen, None, None), None);
}

#[test]
fn labels_and_stop_rules_follow_the_relationships_panel() {
    let edge = |source: &str, target: &str, kind| RelationshipEdge { source: source.into(), target: target.into(), kind, status: None };
    assert_eq!(relationship_label(&edge("me", "kid", RelationshipKind::Subagent), "me", false), "Subagent");
    assert_eq!(relationship_label(&edge("dad", "me", RelationshipKind::Subagent), "me", false), "Parent agent");
    assert_eq!(relationship_label(&edge("me", "f", RelationshipKind::Fork), "me", true), "Side chat");
    assert_eq!(relationship_label(&edge("src", "me", RelationshipKind::Fork), "me", false), "Parent thread");
    assert_eq!(relationship_label(&edge("me", "t", RelationshipKind::Transfer), "me", false), "Context transfer");
    assert_eq!(relationship_status_label(Some("interrupted")), "Stopped");
    assert_eq!(relationship_status_label(None), "Unknown");

    let agent = |extra: Value| decode_agents(vec![subagent("a", Some("r1"), "running", T, extra)]).remove(0);
    assert!(agent(json!({"origin": "app_owned"})).can_stop());
    assert!(!agent(json!({"origin": "app_owned", "startedAt": null})).can_stop(), "not started yet");
    let strong = json!({"origin": "provider_native", "driver": "claudeAgent", "nativeTaskRef": {"strength": "strong", "nativeId": "x"}});
    assert!(agent(strong.clone()).can_stop());
    assert!(!agent(json!({"origin": "provider_native", "driver": "codex", "nativeTaskRef": {"strength": "strong", "nativeId": "x"}})).can_stop());
    assert!(!agent(json!({"origin": "provider_native", "driver": "claudeAgent", "nativeTaskRef": {"strength": "weak", "nativeId": "x"}})).can_stop());
    assert!(agent(merge(strong.clone(), json!({"workflow": {"phases": [], "agents": []}}))).can_stop_workflow());
    assert!(!agent(strong).can_stop_workflow());
}
