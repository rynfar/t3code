use super::*;
use serde_json::json;

fn entry(id: &str, prompt: &str) -> StashEntry {
    StashEntry { id: id.into(), created_at: 1, prompt: prompt.into(), attachments: vec![], threads: vec![], terminals: vec![], review_comments: vec![], context_records: vec![] }
}

fn saved(image: bool) -> SavedAttachment {
    SavedAttachment { id: "a".into(), image, name: "x".into(), mime_type: "image/png".into(), size_bytes: 1, pasted_text: false, attachment_id: "att".into() }
}

#[test]
fn the_stash_keeps_twenty_newest_first() {
    let mut stash = Stash::default();
    for index in 0..MAX_ENTRIES {
        assert_eq!(stash.push(entry(&index.to_string(), "p")), None);
    }
    let evicted = stash.push(entry("new", "p")).unwrap();
    assert_eq!(evicted.id, "0", "the oldest goes");
    assert_eq!(stash.entries[0].id, "new");
    assert_eq!(stash.take("new").unwrap().id, "new");
    assert_eq!(stash.take("missing"), None);
    let restored = Stash::from_value(Some(&stash.to_value()));
    assert_eq!(restored, stash);
    let mut value = stash.to_value();
    value["entries"][0] = json!({ "id": 3 });
    assert_eq!(Stash::from_value(Some(&value)).entries.len(), stash.entries.len() - 1, "a bad entry is dropped alone");
}

#[test]
fn snippets_name_what_was_stashed() {
    assert_eq!(entry("a", "  fix\n the   build ").snippet(), "fix the build");
    assert_eq!(entry("a", &"x".repeat(100)).snippet(), format!("{}…", "x".repeat(90)));
    assert_eq!(entry("a", "").snippet(), "(empty)");
    let with = |attachments| StashEntry { attachments, ..entry("a", " ") };
    assert_eq!(with(vec![saved(true)]).snippet(), "(1 image)");
    assert_eq!(with(vec![saved(false), saved(false)]).snippet(), "(2 files)");
    assert_eq!(with(vec![saved(true), saved(false)]).snippet(), "(2 attachments)");
}

#[test]
fn restored_prompts_join_after_a_blank_line() {
    assert_eq!(merge_prompt("", "stashed"), "stashed");
    assert_eq!(merge_prompt("typed  \n", "stashed"), "typed\n\nstashed");
    assert_eq!(merge_prompt("typed", ""), "typed");
}

#[test]
fn recall_strips_what_the_app_wrote() {
    assert_eq!(recallable_prompt("Ultrathink:\nthink hard"), "think hard");
    assert_eq!(recallable_prompt("compare [T](t3-context://v1/thread/t1) now"), "compare now");
    assert_eq!(recallable_prompt("[T](t3-context://v1/thread/t1) first"), "first");
    assert_eq!(recallable_prompt(ATTACHMENT_ONLY_PROMPT), "");
    assert_eq!(recallable_prompt("PLEASE IMPLEMENT THIS PLAN:\n# Plan"), "");
    assert_eq!(recallable_prompt("see [main.rs](src/main.rs)"), "see [main.rs](src/main.rs)", "file links stay");
}

#[test]
fn history_steps_back_and_forward_from_an_empty_prompt() {
    let entries = vec![HistoryEntry { id: "m1".into(), prompt: "one".into() }, HistoryEntry { id: "m2".into(), prompt: "two".into() }];
    assert_eq!(step_history(true, &entries, None, "typing"), None, "a draft is never replaced");
    let back = step_history(true, &entries, None, "").unwrap();
    assert_eq!(back.prompt, "two");
    let older = step_history(true, &entries, back.position.as_ref(), "two").unwrap();
    assert_eq!(older.prompt, "one");
    assert_eq!(step_history(true, &entries, older.position.as_ref(), "one"), None, "nothing older");
    let newer = step_history(false, &entries, older.position.as_ref(), "one").unwrap();
    assert_eq!(newer.prompt, "two");
    let past_newest = step_history(false, &entries, newer.position.as_ref(), "two").unwrap();
    assert_eq!(past_newest, HistoryStep { position: None, prompt: String::new() });
    assert_eq!(step_history(false, &entries, back.position.as_ref(), "two edited"), None, "an edited recall is a draft");
    assert_eq!(step_history(true, &[], None, ""), None);
}

#[test]
fn caret_edges_follow_lines() {
    assert!(caret_on_edge("one\ntwo", 2, true));
    assert!(!caret_on_edge("one\ntwo", 5, true));
    assert!(caret_on_edge("one\ntwo", 5, false));
    assert!(!caret_on_edge("one\ntwo", 2, false));
    assert!(caret_on_edge("", 0, true) && caret_on_edge("", 0, false));
}

#[test]
fn history_recalls_only_what_the_user_typed() {
    const T: &str = "2026-11-01T00:00:00.000Z";
    let user = |id: &str, text: &str, extra: serde_json::Value| {
        let mut item = json!({
            "id": id, "threadId": "t1", "runId": null, "nodeId": "n1", "parentItemId": null, "ordinal": 1,
            "status": "completed", "title": null, "startedAt": null, "completedAt": null, "updatedAt": T,
            "type": "user_message", "messageId": format!("m-{id}"), "text": text, "inputIntent": "turn_start", "attachments": []
        });
        if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) = (&mut item, extra) {
            base.extend(extra);
        }
        item
    };
    let items = vec![
        user("1", "first", json!({})),
        user("2", "first", json!({})),
        user("3", "from a schedule", json!({ "scheduledTaskId": "task" })),
        user("4", "from an agent", json!({ "createdBy": "agent" })),
        user("5", "from a guest", json!({ "context": { "version": 1, "records": [], "guest": { "name": "A", "text": "hi" } } })),
        user("6", ATTACHMENT_ONLY_PROMPT, json!({})),
        user("7", "second", json!({ "createdBy": "user" })),
    ];
    let visible: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(position, item)| json!({"position": position, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": item["id"], "item": item}))
        .collect();
    let projection: ThreadProjection = serde_json::from_value(json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt"}, "runtimeMode": "full-access",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": items, "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": visible, "updatedAt": T
    }))
    .unwrap();
    let entries = history_entries(&projection);
    let rows: Vec<(&str, &str)> = entries.iter().map(|entry| (entry.id.as_str(), entry.prompt.as_str())).collect();
    assert_eq!(rows, [("m-2", "first"), ("m-7", "second")]);
}
