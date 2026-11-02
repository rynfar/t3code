use super::*;
use serde_json::json;

const T: &str = "2026-11-01T00:00:00.000Z";

fn item(id: &str, kind: Value) -> Value {
    let mut value = json!({
        "id": id, "threadId": "t1", "runId": "r1", "nodeId": "n1", "parentItemId": null, "ordinal": 1,
        "status": "waiting", "title": null, "startedAt": null, "completedAt": null, "updatedAt": T
    });
    if let (Value::Object(base), Value::Object(kind)) = (&mut value, kind) {
        base.extend(kind);
    }
    value
}

fn request(id: &str, kind: &str, status: &str, capability: Value, created_at: &str) -> Value {
    json!({
        "id": id, "nodeId": "n1", "providerTurnId": null, "nativeRequestRef": null, "kind": kind,
        "status": status, "responseCapability": capability, "createdAt": created_at, "resolvedAt": null
    })
}

fn projection(requests: Vec<Value>, items: Vec<Value>) -> ThreadProjection {
    serde_json::from_value(json!({
        "thread": {
            "id": "t1", "projectId": "p1", "title": "Thread", "providerInstanceId": "devReplay",
            "modelSelection": {"instanceId": "devReplay", "model": "showcase"}, "runtimeMode": "approval-required",
            "interactionMode": "default", "branch": null, "worktreePath": null,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1"},
            "createdAt": T, "updatedAt": T, "archivedAt": null
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": requests, "messages": [],
        "plans": [], "turnItems": items, "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": T
    }))
    .unwrap()
}

fn live(feedback: bool) -> Value {
    json!({"type": "live", "providerSessionId": "s1", "supportsFeedback": feedback})
}

#[test]
fn pending_approvals_join_their_turn_item_and_sort_oldest_first() {
    let projection = projection(
        vec![
            request("a2", "command", "pending", live(true), "2026-11-01T00:00:02.000Z"),
            request("a1", "file-change", "pending", json!({"type": "not_resumable", "reason": "gone"}), "2026-11-01T00:00:01.000Z"),
            request("done", "command", "resolved", live(false), T),
            request("tool", "dynamic_tool_call", "pending", live(false), T),
        ],
        vec![
            item("i1", json!({"type": "approval_request", "requestId": "a2", "requestKind": "command",
                              "prompt": "rm -rf build", "reason": "", "options": [
                                  {"decision": "accept", "label": "Run it"},
                                  {"decision": "acceptAlways", "label": "Always", "warning": "Risky"},
                                  {"decision": "bogus", "label": "?"}]})),
        ],
    );
    let pending = pending_requests(&projection);
    let ids: Vec<&str> = pending.approvals.iter().map(|a| a.request_id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "a2"]);
    let (first, second) = (&pending.approvals[0], &pending.approvals[1]);
    assert!(!first.live && !first.supports_feedback);
    assert_eq!(first.options, default_approval_options());
    assert_eq!(first.presentation(), ("File change approval", "File change"));
    assert!(second.live && second.supports_feedback);
    assert_eq!(second.detail.as_deref(), Some("rm -rf build"));
    assert_eq!(second.reason, None);
    assert_eq!(second.options.len(), 2);
    let (primary, more) = second.split_options();
    assert_eq!(primary.len(), 1);
    assert_eq!(more[0].warning.as_deref(), Some("Risky"));
}

#[test]
fn pending_questions_need_their_turn_item_and_message_mode_is_dismissible() {
    let projection = projection(
        vec![
            request("q1", "user_input", "pending", json!({"type": "message"}), T),
            request("q2", "user_input", "pending", live(false), T),
        ],
        vec![item("i1", json!({"type": "user_input_request", "requestId": "q1", "questions": [
            {"id": "color", "header": "Color", "question": "Which color?",
             "options": [{"label": "Red", "description": "Warm"}, {"label": "Blue", "description": "Cool", "value": "blue"}]}
        ]}))],
    );
    let pending = pending_requests(&projection);
    assert_eq!(pending.user_inputs.len(), 1);
    let input = &pending.user_inputs[0];
    assert!(input.dismissible && input.can_respond());
    assert!(!input.questions[0].multi_select);
    assert_eq!(input.questions[0].options[1].answer_value(), "blue");
}

fn question(multi: bool, custom: Option<bool>, initial: Option<&str>) -> Question {
    serde_json::from_value(json!({
        "id": "q", "header": "H", "question": "?", "multiSelect": multi, "allowCustomAnswer": custom,
        "initialAnswer": initial,
        "options": [{"label": "Red", "description": "d"}, {"label": "Blue", "description": "d", "value": " blue "}]
    }))
    .unwrap()
}

#[test]
fn single_select_takes_the_pick_and_custom_text_wins() {
    let q = question(false, None, None);
    assert_eq!(resolve_answer(&q, None), None);
    let picked = toggle_option(&q, None, "Red");
    assert_eq!(resolve_answer(&q, Some(&picked)), Some(Answer::Text("Red".into())));
    assert!(is_option_selected(&q, Some(&picked), "Red"));
    let exact = toggle_option(&q, Some(&picked), " blue ");
    assert_eq!(resolve_answer(&q, Some(&exact)), Some(Answer::Text(" blue ".into())));
    assert_eq!(toggle_option(&q, Some(&exact), "Green"), exact);
    let typed = set_custom_answer(&q, Some(&exact), "  purple ");
    assert_eq!(resolve_answer(&q, Some(&typed)), Some(Answer::Text("purple".into())));
    assert!(!is_option_selected(&q, Some(&typed), " blue "));
    let blank = set_custom_answer(&q, Some(&exact), "   ");
    assert_eq!(resolve_answer(&q, Some(&blank)), Some(Answer::Text(" blue ".into())));
}

#[test]
fn multi_select_toggles_and_choice_only_ignores_text() {
    let q = question(true, None, None);
    let one = toggle_option(&q, None, "Red");
    let two = toggle_option(&q, Some(&one), " blue ");
    assert_eq!(resolve_answer(&q, Some(&two)), Some(Answer::Choices(vec!["Red".into(), " blue ".into()])));
    let back = toggle_option(&q, Some(&two), "Red");
    assert_eq!(back.selected, vec![" blue ".to_owned()]);
    let choice_only = question(false, Some(false), None);
    let typed = set_custom_answer(&choice_only, None, "nope");
    assert_eq!(resolve_answer(&choice_only, Some(&typed)), None);
}

#[test]
fn suggested_answers_seed_drafts_and_survive_clearing() {
    let q = question(false, None, Some("Red-ish"));
    let mut drafts = HashMap::new();
    seed_drafts(std::slice::from_ref(&q), &mut drafts);
    assert_eq!(resolve_answer(&q, drafts.get("q")), Some(Answer::Text("Red-ish".into())));
    let cleared = set_custom_answer(&q, drafts.get("q"), "");
    assert_eq!(resolve_answer(&q, Some(&cleared)), Some(Answer::Text(String::new())));
}

#[test]
fn answers_build_only_when_every_question_is_answered() {
    let first = question(false, None, None);
    let mut second = question(true, None, None);
    second.id = "q2".into();
    let questions = vec![first.clone(), second.clone()];
    let mut drafts = HashMap::new();
    drafts.insert("q".to_owned(), toggle_option(&first, None, "Red"));
    let partial = progress(&questions, &drafts, 0);
    assert!(partial.can_advance && !partial.is_last_question && !partial.is_complete);
    assert_eq!(primary_action_label(&partial, false), "Next question");
    assert!(build_answers(&questions, &drafts).is_none());
    drafts.insert("q2".to_owned(), toggle_option(&second, None, " blue "));
    let done = progress(&questions, &drafts, 5);
    assert_eq!(done.question_index, 1);
    assert!(done.is_complete && done.is_last_question);
    assert_eq!(primary_action_label(&done, false), "Submit answers");
    assert_eq!(primary_action_label(&done, true), "Submitting...");
    let answers = build_answers(&questions, &drafts).unwrap();
    assert_eq!(Value::Object(answers), json!({"q": "Red", "q2": [" blue "]}));
}

#[test]
fn displaced_answers_join_the_draft_after_a_blank_line() {
    assert_eq!(carry_displaced_answer("draft  ", "  typed "), "draft\n\ntyped");
    assert_eq!(carry_displaced_answer("  ", "typed"), "typed");
    assert_eq!(carry_displaced_answer("draft", "   "), "draft");
}
