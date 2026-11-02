use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::timeline::{EntriesCache, EntryKind, derive_entries};

fn item(body: Value) -> TurnItem {
    let mut item = json!({
        "id": "item", "threadId": "thread", "runId": "run", "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
        "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-10-01T00:00:00Z",
    });
    item.as_object_mut().expect("object").extend(body.as_object().expect("object").clone());
    serde_json::from_value(item).expect("item decodes")
}

/// The work entry the transcript derives for one item.
fn entry(body: Value) -> Arc<WorkLogEntry> {
    let item = serde_json::to_value(item(body)).expect("encodes");
    let projected: crate::thread::ProjectedTurnItem = serde_json::from_value(json!({
        "position": 0, "visibility": "local", "sourceThreadId": "thread", "sourceItemId": "item", "item": item,
    }))
    .expect("projected item decodes");
    let entries = derive_entries(&mut EntriesCache::default(), &[projected], &[], &[], &[]);
    entries
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::Work(work) => Some(work.clone()),
            _ => None,
        })
        .expect("a work entry")
}

fn command(extra: Value) -> Value {
    let mut body = json!({ "type": "command_execution", "input": "cargo test" });
    body.as_object_mut().expect("object").extend(extra.as_object().expect("object").clone());
    body
}

#[test]
fn a_fetched_item_stands_in_for_the_wire_item_once_it_lands() {
    let wire = item(command(json!({ "outputOmitted": true })));
    let fetched = item(command(json!({ "output": "ok\n", "exitCode": 0 })));
    let line = |output: &ToolOutput| output.line().map(|line| line.copy());

    let (shown, output) = fetched_turn_item(&wire, true, None);
    assert!(std::ptr::eq(shown, &wire));
    assert_eq!(line(&output).as_deref(), Some("Loading output…"));

    let (shown, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Loaded(&fetched)));
    assert!(std::ptr::eq(shown, &fetched));
    assert_eq!(line(&output).as_deref(), Some("ok\n"));

    let empty = item(command(json!({})));
    let (_, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Loaded(&empty)));
    assert_eq!(line(&output).as_deref(), Some("No output."));

    let (_, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Missing));
    assert_eq!(line(&output).as_deref(), Some("Couldn't load output: Output is no longer available."));

    let (_, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Failed("socket closed")));
    assert_eq!(line(&output).as_deref(), Some("Couldn't load output: socket closed"));

    let other = item(json!({ "type": "dynamic_tool", "toolName": "x", "input": {}, "output": "x" }));
    let (shown, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Loaded(&other)));
    assert!(std::ptr::eq(shown, &wire), "another kind of item is ignored");
    assert_eq!(line(&output), None);

    let plain = item(command(json!({ "output": "inline" })));
    let (_, output) = fetched_turn_item(&plain, false, None);
    assert_eq!(line(&output).as_deref(), Some("inline"));
    let (_, output) = fetched_turn_item(&item(command(json!({}))), false, None);
    assert_eq!(line(&output), None, "nothing to fetch and nothing printed");
}

#[test]
fn images_alone_need_no_empty_note() {
    let wire = item(json!({ "type": "dynamic_tool", "toolName": "shot", "input": {}, "outputOmitted": true }));
    let fetched = item(json!({ "type": "dynamic_tool", "toolName": "shot", "input": {}, "output": [{ "type": "image", "mimeType": "image/png" }] }));
    let (_, output) = fetched_turn_item(&wire, true, Some(FetchedDetail::Loaded(&fetched)));
    assert_eq!(output.images.len(), 1);
    assert_eq!(output.line(), None);
}

#[test]
fn reads_and_skills_show_plain_output_and_other_tools_the_inspector() {
    let read = entry(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "/repo/src/a.rs" } }));
    assert_eq!(entry_body(&read, Some("/repo")), Some(EntryBody::Plain { text: Some("/repo/src/a.rs".into()), fetches: false }));

    let withheld = entry(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "src/a.rs" }, "outputOmitted": true }));
    assert_eq!(entry_body(&withheld, Some("/repo")), Some(EntryBody::Plain { text: Some("/repo/src/a.rs".into()), fetches: true }));

    let skill = entry(json!({ "type": "dynamic_tool", "toolName": "Skill", "input": { "skill": "review", "args": "--strict" } }));
    assert_eq!(entry_body(&skill, None), Some(EntryBody::Plain { text: Some("--strict".into()), fetches: false }));
    let bare_skill = entry(json!({ "type": "dynamic_tool", "toolName": "Skill", "input": { "skill": "review" } }));
    assert!(!entry_can_expand(&bare_skill, None), "a skill without arguments opens to nothing");
    assert_eq!(entry_body(&bare_skill, None), None);

    let mcp = entry(json!({ "type": "dynamic_tool", "toolName": "linear.search", "input": { "query": "bug" } }));
    assert_eq!(entry_body(&mcp, None), Some(EntryBody::Inspector));
    let bare = entry(json!({ "type": "dynamic_tool", "toolName": "linear.ping", "input": {} }));
    assert!(!entry_can_expand(&bare, None));

    let asked = entry(json!({
        "type": "user_input_request", "requestId": "r", "questions": [{ "id": "q1", "question": "Which?", "options": [] }],
        "questionAnswer": { "requestId": "r", "questionTextById": { "q1": "Which?" }, "answers": { "q1": "This" }, "attachmentsByQuestionId": {} },
    }));
    assert_eq!(entry_body(&asked, None), Some(EntryBody::QuestionAnswer));
}

#[test]
fn only_open_rows_with_withheld_detail_fetch_and_streamed_commands_do_not() {
    let withheld = entry(command(json!({ "outputOmitted": true })));
    assert!(entry_fetches_detail(&withheld, false));
    assert!(!entry_fetches_detail(&withheld, true), "the server streams the output instead");
    assert!(!entry_fetches_detail(&entry(command(json!({ "output": "x" }))), false));
    let read = entry(json!({ "type": "dynamic_tool", "toolName": "Read", "input": { "file_path": "a.rs" }, "outputOmitted": true }));
    assert!(entry_fetches_detail(&read, true));
    let summarized = entry(json!({ "type": "dynamic_tool", "toolName": "linear.search", "input": { "truncated": true, "summary": "{…}" } }));
    assert!(entry_fetches_detail(&summarized, true));
}

#[test]
fn inspector_text_lines_match_the_web() {
    let questions = [json!({ "question": "Pick", "options": [{ "label": "A" }, { "label": "B" }] }), json!({ "question": "Why?" })];
    assert_eq!(questions_text(&questions), "Pick\n• A\n• B\n\nWhy?");
    let step = |status: &str| crate::turn_item::PlanStep { id: status.into(), text: format!("{status} step"), status: status.into() };
    assert_eq!(todo_text(&[step("completed"), step("running"), step("pending")]), "✓ completed step\n◉ running step\n○ pending step");
    let change = FileChangeDetail {
        operation: "rename".into(),
        path: "/repo/b.png".into(),
        old_path: Some("a.png".into()),
        file_type: Some("binary".into()),
        mime_type: Some("image/png".into()),
    };
    assert_eq!(file_change_line(&change, Some("/repo")), "rename a.png → repo/b.png (binary, image/png)");
    assert_eq!(search_result_location(&json!({ "fileName": "/repo/a.rs", "line": 4, "column": 9 }), Some("/repo")), "repo/a.rs:4:9");
    assert_eq!(external_web_link(Some("//example.com/a")).as_deref(), Some("https://example.com/a"));
    assert_eq!(external_web_link(Some("https://example.com")).as_deref(), Some("https://example.com/"));
    assert_eq!(external_web_link(Some("javascript:alert(1)")), None);
    assert_eq!(external_web_link(Some("not a url")), None);
    assert_eq!(external_web_link(None), None);
}

#[test]
fn answered_questions_list_every_question_once_in_order() {
    let answer = json!({
        "requestId": "r",
        "questionTextById": { "q2": "Second?", "q1": "First?" },
        "answers": { "q1": { "answers": ["A", "B"] }, "q3": "Only answered" },
        "attachmentsByQuestionId": { "q2": [{ "id": "f", "name": "shot.png", "type": "image" }, { "id": "g", "name": "notes.txt", "type": "file" }] },
    });
    assert_eq!(
        answered_questions(&answer),
        vec![
            AnsweredQuestion {
                question: Some("Second?".into()),
                answer: None,
                attachments: vec![
                    AnswerAttachment { id: "f".into(), name: "shot.png".into(), image: true },
                    AnswerAttachment { id: "g".into(), name: "notes.txt".into(), image: false },
                ],
            },
            AnsweredQuestion { question: Some("First?".into()), answer: Some("A, B".into()), attachments: vec![] },
            AnsweredQuestion { question: None, answer: Some("Only answered".into()), attachments: vec![] },
        ]
    );
}
