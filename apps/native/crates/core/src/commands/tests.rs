use super::*;

fn selection() -> ModelSelection {
    ModelSelection { instance_id: "devReplay".into(), model: "showcase".into(), options: None }
}

#[test]
fn send_message_matches_message_dispatch_schema() {
    let command = send_message("c1", "t1", "hi", &DispatchMode::StartImmediately);
    assert_eq!(command["type"], "message.dispatch");
    assert_eq!(command["messageId"], "c1");
    assert_eq!(command["dispatchMode"], json!({ "type": "start_immediately" }));
    assert_eq!(command["creationSource"], "web");
    let steer = send_message("c2", "t1", "x", &DispatchMode::SteerActive { target_run_id: "r1".into() });
    assert_eq!(steer["dispatchMode"], json!({ "type": "steer_active", "targetRunId": "r1" }));
}

#[test]
fn composer_message_steers_with_delivery_intent_and_queues_without_it() {
    let selection = selection();
    let message = |dispatch| ComposerMessage {
        command_id: "c1",
        thread_id: "t1",
        message_id: "m1",
        text: "go",
        title_seed: "go",
        model_selection: &selection,
        dispatch,
    };
    let steer = composer_message(&message(ComposerDispatch::Steer));
    assert_eq!(steer["dispatchMode"], json!({ "type": "start_immediately" }));
    assert_eq!(steer["deliveryIntent"], "steer");
    assert_eq!(steer["modelSelection"], json!({ "instanceId": "devReplay", "model": "showcase" }));
    let auto = composer_message(&message(ComposerDispatch::Auto));
    assert_eq!(auto["deliveryIntent"], "auto");
    let queue = composer_message(&message(ComposerDispatch::Queue));
    assert_eq!(queue["dispatchMode"], json!({ "type": "queue_after_active" }));
    assert!(queue.get("deliveryIntent").is_none());
}

#[test]
fn launch_thread_matches_thread_launch_input() {
    let selection = selection();
    let launch = launch_thread(&LaunchThread {
        command_id: "c1",
        thread_id: "t1",
        project_id: "p1",
        title: "Fix the build",
        model_selection: &selection,
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Default,
        message_id: "m1",
        text: "Fix the build",
    });
    assert_eq!(launch["runtimeMode"], "approval-required");
    assert_eq!(launch["interactionMode"], "default");
    assert_eq!(launch["workspaceStrategy"], json!({ "type": "root" }));
    assert_eq!(launch["generateTitle"], true);
    assert_eq!(launch["initialMessage"], json!({ "messageId": "m1", "text": "Fix the build", "attachments": [] }));
    assert!(launch.get("reuseExistingThread").is_none());
}

#[test]
fn runtime_request_responses_match_the_respond_schema() {
    let approve = respond_to_approval("c1", "t1", "r1", ApprovalDecision::AcceptForSession, None);
    assert_eq!(approve["type"], "runtime-request.respond");
    assert_eq!(approve["decision"], "acceptForSession");
    assert!(approve.get("answers").is_none() && approve.get("feedback").is_none());
    let feedback = respond_to_approval("c1", "t1", "r1", ApprovalDecision::Decline, Some("use rg instead"));
    assert_eq!(feedback["feedback"], "use rg instead");
    let mut answers = Map::new();
    answers.insert("q1".into(), json!(["a", "b"]));
    let answer = respond_to_user_input("c2", "t1", "r2", answers);
    assert_eq!(answer["answers"], json!({ "q1": ["a", "b"] }));
    assert!(answer.get("decision").is_none());
    assert_eq!(dismiss_user_input("c3", "t1", "r2")["type"], "thread.user-input.dismiss");
}

#[test]
fn thread_setting_commands_use_wire_literals() {
    assert_eq!(set_runtime_mode("c", "t", RuntimeMode::AutoAcceptEdits)["runtimeMode"], "auto-accept-edits");
    assert_eq!(set_interaction_mode("c", "t", InteractionMode::Plan)["interactionMode"], "plan");
    assert_eq!(set_model_selection("c", "t", &selection())["type"], "thread.model-selection.set");
    assert_eq!(ApprovalDecision::parse("acceptAlways"), Some(ApprovalDecision::AcceptAlways));
}
