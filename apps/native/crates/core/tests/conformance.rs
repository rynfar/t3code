//! Replays the fixtures `apps/native/conformance/generate.ts` writes from the real TypeScript
//! reducers and server wire encoders, and compares what the client can observe: rows, their order
//! and positions, item bodies, records, thread fields, history meta, and the shell.
//! Regenerate with `node apps/native/conformance/generate.ts`.

use serde_json::Value;
use t3_core::history::{HistoryStep, decode_history_page};
use t3_core::shell::{ShellSnapshot, ShellState, ShellStreamItem};
use t3_core::thread::{
    ApplyOptions, DomainEvent, Entity, ThreadProjection, ThreadState, Visibility, apply_event, decode_thread_stream_item,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};

/// One case per line.
fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/tests/conformance/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let cases: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).expect("each line is a JSON case")).collect();
    assert!(!cases.is_empty(), "{path} has no cases");
    cases
}

fn s(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

#[derive(Debug, PartialEq)]
struct ItemView {
    id: String,
    kind: String,
    status: String,
    ordinal: u64,
    run_id: Option<String>,
    title: Option<String>,
    text: Option<String>,
    text_omitted: bool,
    output: Option<String>,
    files: Option<Vec<(String, u64, u64)>>,
    files_omitted: bool,
    prompt: Option<String>,
    result: Option<String>,
    prompt_omitted: bool,
    result_omitted: bool,
}

fn kind_name(kind: &TurnItemKind) -> &'static str {
    match kind {
        TurnItemKind::Notification { .. } => "notification",
        TurnItemKind::UserMessage { .. } => "user_message",
        TurnItemKind::AssistantMessage { .. } => "assistant_message",
        TurnItemKind::Reasoning { .. } => "reasoning",
        TurnItemKind::ProposedPlan { .. } => "proposed_plan",
        TurnItemKind::TodoList { .. } => "todo_list",
        TurnItemKind::UserInputRequest { .. } => "user_input_request",
        TurnItemKind::FileChange { .. } => "file_change",
        TurnItemKind::CommandExecution { .. } => "command_execution",
        TurnItemKind::FileSearch { .. } => "file_search",
        TurnItemKind::WebSearch { .. } => "web_search",
        TurnItemKind::ApprovalRequest { .. } => "approval_request",
        TurnItemKind::Checkpoint { .. } => "checkpoint",
        TurnItemKind::RunInterruptRequest { .. } => "run_interrupt_request",
        TurnItemKind::RunInterruptResult { .. } => "run_interrupt_result",
        TurnItemKind::SystemNotice { .. } => "system_notice",
        TurnItemKind::Error { .. } => "error",
        TurnItemKind::Compaction { .. } => "compaction",
        TurnItemKind::Handoff { .. } => "handoff",
        TurnItemKind::Fork { .. } => "fork",
        TurnItemKind::ThreadCreated { .. } => "thread_created",
        TurnItemKind::SecretRequest { .. } => "secret_request",
        TurnItemKind::LinkRequest { .. } => "link_request",
        TurnItemKind::Subagent { .. } => "subagent",
        TurnItemKind::DynamicTool { .. } => "dynamic_tool",
        TurnItemKind::Unknown => "unknown",
    }
}

impl ItemView {
    fn from_json(item: &Value) -> Self {
        let kind = s(item, "type").unwrap_or_default();
        let has = |kinds: &[&str]| kinds.contains(&kind.as_str());
        ItemView {
            id: s(item, "id").unwrap_or_default(),
            status: s(item, "status").unwrap_or_default(),
            ordinal: item.get("ordinal").and_then(Value::as_u64).unwrap_or_default(),
            run_id: s(item, "runId"),
            title: s(item, "title"),
            text: has(&["user_message", "assistant_message", "reasoning"]).then(|| s(item, "text")).flatten(),
            text_omitted: flag(item, "textOmitted"),
            output: has(&["command_execution"]).then(|| s(item, "output")).flatten(),
            files: has(&["checkpoint"]).then(|| {
                item.get("files").and_then(Value::as_array).into_iter().flatten().map(file_view).collect()
            }),
            files_omitted: flag(item, "filesOmitted"),
            prompt: has(&["subagent"]).then(|| s(item, "prompt")).flatten(),
            result: has(&["subagent"]).then(|| s(item, "result")).flatten(),
            prompt_omitted: flag(item, "promptOmitted"),
            result_omitted: flag(item, "resultOmitted"),
            kind,
        }
    }

    fn from_state(item: &TurnItem) -> Self {
        let mut view = ItemView {
            id: item.id.clone(),
            kind: kind_name(&item.kind).to_owned(),
            status: item.status.clone(),
            ordinal: item.ordinal,
            run_id: item.run_id.clone(),
            title: item.title.clone(),
            text: None,
            text_omitted: item.text_omitted,
            output: None,
            files: None,
            files_omitted: item.files_omitted,
            prompt: None,
            result: None,
            prompt_omitted: item.prompt_omitted,
            result_omitted: item.result_omitted,
        };
        match &item.kind {
            TurnItemKind::UserMessage { text, .. }
            | TurnItemKind::AssistantMessage { text, .. }
            | TurnItemKind::Reasoning { text, .. } => view.text = Some(text.clone()),
            TurnItemKind::CommandExecution { output, .. } => view.output = output.clone(),
            TurnItemKind::Checkpoint { files, .. } => {
                view.files = Some(files.iter().map(|f| (f.path.clone(), f.additions, f.deletions)).collect());
            }
            TurnItemKind::Subagent { prompt, result, .. } => {
                view.prompt = Some(prompt.clone());
                view.result = result.clone();
            }
            _ => {}
        }
        view
    }
}

fn file_view(file: &Value) -> (String, u64, u64) {
    let number = |key| file.get(key).and_then(Value::as_u64).unwrap_or_default();
    (s(file, "path").unwrap_or_default(), number("additions"), number("deletions"))
}

#[derive(Debug, PartialEq)]
struct RowView {
    position: usize,
    visibility: String,
    source_thread_id: String,
    source_item_id: String,
    item: ItemView,
}

#[derive(Debug, PartialEq)]
struct ProjectionView {
    updated_at: String,
    thread: Vec<Option<String>>,
    runs: Vec<(String, String, Option<Value>)>,
    attempts: Vec<(String, String, String, String)>,
    turn_items: Vec<ItemView>,
    visible: Vec<RowView>,
    messages: Vec<(String, Option<String>)>,
    subagents: Vec<Vec<Option<Value>>>,
    provider_turns: Vec<(String, Option<Value>, Option<Value>)>,
    entity_ids: Vec<Vec<String>>,
}

const SUBAGENT_KEYS: [&str; 6] = ["id", "prompt", "result", "progress", "promptOmitted", "resultOmitted"];
const ENTITY_KEYS: [&str; 9] = [
    "nodes",
    "providerSessions",
    "providerThreads",
    "runtimeRequests",
    "plans",
    "checkpointScopes",
    "checkpoints",
    "contextHandoffs",
    "contextTransfers",
];

fn list<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn field(value: &Value, key: &str) -> Option<Value> {
    value.get(key).filter(|v| !v.is_null()).cloned()
}

impl ProjectionView {
    fn from_json(p: &Value) -> Self {
        let thread = &p["thread"];
        ProjectionView {
            updated_at: s(p, "updatedAt").unwrap_or_default(),
            thread: ["id", "title", "archivedAt", "updatedAt", "branch", "worktreePath", "runtimeMode", "interactionMode"]
                .iter()
                .map(|key| s(thread, key))
                .collect(),
            runs: list(p, "runs")
                .map(|r| (s(r, "id").unwrap_or_default(), s(r, "status").unwrap_or_default(), field(r, "restartCancelledBackgroundWork")))
                .collect(),
            attempts: list(p, "attempts")
                .map(|a| {
                    let get = |key| s(a, key).unwrap_or_default();
                    (get("id"), get("runId"), get("rootNodeId"), get("status"))
                })
                .collect(),
            turn_items: list(p, "turnItems").map(ItemView::from_json).collect(),
            visible: list(p, "visibleTurnItems")
                .map(|row| RowView {
                    position: row.get("position").and_then(Value::as_u64).unwrap_or_default() as usize,
                    visibility: s(row, "visibility").unwrap_or_default(),
                    source_thread_id: s(row, "sourceThreadId").unwrap_or_default(),
                    source_item_id: s(row, "sourceItemId").unwrap_or_default(),
                    item: ItemView::from_json(&row["item"]),
                })
                .collect(),
            messages: list(p, "messages").map(|m| (s(m, "id").unwrap_or_default(), s(m, "text"))).collect(),
            subagents: list(p, "subagents").map(|v| SUBAGENT_KEYS.iter().map(|k| field(v, k)).collect()).collect(),
            provider_turns: list(p, "providerTurns")
                .map(|t| (s(t, "id").unwrap_or_default(), field(t, "status"), field(t, "tokenUsage")))
                .collect(),
            entity_ids: ENTITY_KEYS
                .iter()
                .map(|key| list(p, key).map(|e| s(e, "id").unwrap_or_default()).collect())
                .collect(),
        }
    }

    fn from_state(p: &ThreadProjection) -> Self {
        let t = &p.thread;
        let entities = |items: &[Entity]| items.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        ProjectionView {
            updated_at: p.updated_at.clone(),
            thread: vec![
                Some(t.id.clone()),
                Some(t.title.clone()),
                t.archived_at.clone(),
                Some(t.updated_at.clone()),
                t.branch.clone(),
                t.worktree_path.clone(),
                Some(t.runtime_mode.clone()),
                Some(t.interaction_mode.clone()),
            ],
            runs: p
                .runs
                .iter()
                .map(|r| (r.id.clone(), r.status.clone(), r.restart_cancelled_background_work.clone().filter(|v| !v.is_null())))
                .collect(),
            attempts: p
                .attempts
                .iter()
                .map(|a| (a.id.clone(), a.run_id.clone(), a.root_node_id.clone(), a.status.clone()))
                .collect(),
            turn_items: p.turn_items.iter().map(|item| ItemView::from_state(item)).collect(),
            visible: p
                .visible_turn_items
                .iter()
                .map(|row| RowView {
                    position: row.position,
                    visibility: match row.visibility {
                        Visibility::Local => "local",
                        Visibility::Inherited => "inherited",
                        Visibility::Synthetic => "synthetic",
                    }
                    .to_owned(),
                    source_thread_id: row.source_thread_id.clone(),
                    source_item_id: row.source_item_id.clone(),
                    item: ItemView::from_state(&row.item),
                })
                .collect(),
            messages: p.messages.iter().map(|m| (m.id.clone(), s(&m.value, "text"))).collect(),
            subagents: p.subagents.iter().map(|e| SUBAGENT_KEYS.iter().map(|k| field(&e.value, k)).collect()).collect(),
            provider_turns: p
                .provider_turns
                .iter()
                .map(|e| (e.id.clone(), field(&e.value, "status"), field(&e.value, "tokenUsage")))
                .collect(),
            entity_ids: vec![
                entities(&p.nodes),
                entities(&p.provider_sessions),
                entities(&p.provider_threads),
                entities(&p.runtime_requests),
                entities(&p.plans),
                entities(&p.checkpoint_scopes),
                entities(&p.checkpoints),
                entities(&p.context_handoffs),
                entities(&p.context_transfers),
            ],
        }
    }
}

/// Collects every divergence so one run reports all of them.
#[derive(Default)]
struct Report(Vec<String>);

impl Report {
    fn check<T: PartialEq + std::fmt::Debug>(&mut self, case: &str, what: &str, rust: T, typescript: T) {
        if rust != typescript {
            self.0.push(format!("{case}: {what}\n  rust:       {rust:?}\n  typescript: {typescript:?}"));
        }
    }

    fn finish(self, family: &str) {
        assert!(self.0.is_empty(), "{} {family} divergence(s):\n{}", self.0.len(), self.0.join("\n\n"));
    }
}

fn compare_projection(report: &mut Report, case: &str, rust: Option<&ThreadProjection>, expected: &Value) {
    match (rust, expected.is_null()) {
        (None, true) => {}
        (Some(projection), false) => {
            let (rust, typescript) = (ProjectionView::from_state(projection), ProjectionView::from_json(expected));
            report.check(case, "updatedAt", &rust.updated_at, &typescript.updated_at);
            report.check(case, "thread", &rust.thread, &typescript.thread);
            report.check(case, "runs", &rust.runs, &typescript.runs);
            report.check(case, "attempts", &rust.attempts, &typescript.attempts);
            report.check(case, "visible row ids", ids(&rust.visible), ids(&typescript.visible));
            report.check(case, "visible rows", &rust.visible, &typescript.visible);
            report.check(case, "turn item ids", item_ids(&rust.turn_items), item_ids(&typescript.turn_items));
            report.check(case, "turn items", &rust.turn_items, &typescript.turn_items);
            report.check(case, "messages", &rust.messages, &typescript.messages);
            report.check(case, "subagents", &rust.subagents, &typescript.subagents);
            report.check(case, "provider turns", &rust.provider_turns, &typescript.provider_turns);
            report.check(case, "entity ids", &rust.entity_ids, &typescript.entity_ids);
        }
        (rust, _) => report.check(case, "projection present", rust.is_some(), !expected.is_null()),
    }
}

fn ids(rows: &[RowView]) -> Vec<&str> {
    rows.iter().map(|row| row.source_item_id.as_str()).collect()
}

fn item_ids(items: &[ItemView]) -> Vec<&str> {
    items.iter().map(|item| item.id.as_str()).collect()
}

fn name(case: &Value) -> String {
    s(case, "name").unwrap_or_default()
}

#[test]
fn reducer_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("reducer.jsonl") {
        let name = name(&case);
        let mut projection: ThreadProjection =
            serde_json::from_value(case["initial"].clone()).unwrap_or_else(|e| panic!("{name}: initial: {e}"));
        for step in case["steps"].as_array().into_iter().flatten() {
            let event: DomainEvent =
                serde_json::from_value(step["event"].clone()).unwrap_or_else(|e| panic!("{name}: event: {e}"));
            let options = match step.get("options").filter(|o| !o.is_null()) {
                Some(options) => ApplyOptions {
                    partial_timeline: flag(options, "partialTimeline"),
                    latest_local_turn_ordinal: options.get("latestLocalTurnOrdinal").and_then(Value::as_u64),
                },
                None => ApplyOptions::default(),
            };
            apply_event(&mut projection, event, options);
        }
        compare_projection(&mut report, &name, Some(&projection), &case["expected"]);
    }
    report.finish("reducer");
}

fn step_name(step: HistoryStep) -> String {
    match step {
        HistoryStep::Continue(cursor) => format!("continue:{cursor}"),
        HistoryStep::Done => "done".into(),
        HistoryStep::Stale => "stale".into(),
        HistoryStep::Failed(message) => format!("failed:{message}"),
    }
}

#[test]
fn thread_stream_and_history_match_typescript() {
    let mut report = Report::default();
    for case in fixture("thread.jsonl") {
        let name = name(&case);
        let mut state = ThreadState::default();
        for (index, op) in case["ops"].as_array().into_iter().flatten().enumerate() {
            let at = format!("op {index}");
            if let Some(item) = op.get("stream") {
                let item = decode_thread_stream_item(item.clone()).unwrap_or_else(|e| panic!("{name}: {at}: {e}"));
                state.apply(item);
            } else if let Some(expected) = op.get("begin") {
                report.check(&name, &format!("{at} begin"), state.begin_history_load(), s(op, "begin").filter(|_| !expected.is_null()));
            } else if let Some(page) = op.get("page") {
                let page = decode_history_page(page.clone()).unwrap_or_else(|e| panic!("{name}: {at}: {e}"));
                let cursor = s(op, "requestCursor").unwrap_or_default();
                report.check(&name, &format!("{at} page"), step_name(state.apply_history_page(&cursor, page)), s(op, "step").unwrap_or_default());
            } else if let Some(message) = s(op, "fail") {
                let cursor = s(op, "requestCursor").unwrap_or_default();
                report.check(&name, &format!("{at} fail"), step_name(state.fail_history_load(&cursor, message)), s(op, "step").unwrap_or_default());
            }
        }
        let expected = &case["expected"];
        compare_projection(&mut report, &name, state.projection.as_ref(), &expected["projection"]);
        report.check(&name, "sequence", state.sequence, expected["sequence"].as_u64());
        let history = &expected["history"];
        let meta = &state.history;
        report.check(&name, "history cursor", meta.history_cursor.clone(), s(history, "historyCursor"));
        report.check(&name, "has more history", meta.has_more_history, flag(history, "hasMoreHistory"));
        report.check(&name, "history loading", meta.loading, flag(history, "loading"));
        report.check(&name, "history error", meta.error.clone(), s(history, "error"));
        report.check(&name, "history expanded", meta.expanded, flag(history, "expanded"));
        report.check(&name, "watermark", meta.latest_local_turn_ordinal, history["latestLocalTurnOrdinal"].as_u64());
    }
    report.finish("thread");
}

#[derive(Debug, PartialEq)]
struct ShellView {
    sequence: u64,
    projects: Vec<(String, String, String, Option<Value>)>,
    threads: Vec<Vec<Option<String>>>,
    archived: Vec<String>,
}

const THREAD_KEYS: [&str; 6] = ["id", "projectId", "title", "status", "activeRunId", "archivedAt"];

impl ShellView {
    fn from_json(snapshot: &Value) -> Self {
        ShellView {
            sequence: snapshot["snapshotSequence"].as_u64().unwrap_or_default(),
            projects: list(snapshot, "projects")
                .map(|p| {
                    let get = |key| s(p, key).unwrap_or_default();
                    (get("id"), get("title"), get("workspaceRoot"), field(p, "repositoryIdentity"))
                })
                .collect(),
            threads: list(snapshot, "threads").map(|t| THREAD_KEYS.iter().map(|k| s(t, k)).collect()).collect(),
            archived: list(snapshot, "archivedThreads").map(|t| s(t, "id").unwrap_or_default()).collect(),
        }
    }

    fn from_state(snapshot: &ShellSnapshot) -> Self {
        ShellView {
            sequence: snapshot.snapshot_sequence,
            projects: snapshot
                .projects
                .iter()
                .map(|p| (p.id.clone(), p.title.clone(), p.workspace_root.clone(), p.repository_identity.clone()))
                .collect(),
            threads: snapshot
                .threads
                .iter()
                .map(|t| {
                    vec![
                        Some(t.id.clone()),
                        Some(t.project_id.clone()),
                        Some(t.title.clone()),
                        Some(t.status.clone()),
                        t.active_run_id.clone(),
                        t.archived_at.clone(),
                    ]
                })
                .collect(),
            archived: snapshot.archived_threads.iter().map(|t| t.id.clone()).collect(),
        }
    }
}

#[test]
fn shell_matches_typescript() {
    let mut report = Report::default();
    for case in fixture("shell.jsonl") {
        let name = name(&case);
        let mut state = ShellState::default();
        for item in case["items"].as_array().into_iter().flatten() {
            let item: ShellStreamItem = serde_json::from_value(item.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            state.apply(item);
        }
        let expected = &case["expected"];
        report.check(
            &name,
            "shell",
            state.snapshot.as_ref().map(ShellView::from_state),
            (!expected.is_null()).then(|| ShellView::from_json(expected)),
        );
    }
    report.finish("shell");
}
