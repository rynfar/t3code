use super::*;
use serde_json::json;

#[test]
fn request_matches_effect_rpc_encoding() {
    let message = ClientMessage::Request {
        id: 7.into(),
        tag: "orchestration.subscribeShell".into(),
        payload: json!({}),
        headers: vec![],
    };
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({"_tag":"Request","id":"7","tag":"orchestration.subscribeShell","payload":{},"headers":[]})
    );
    assert_eq!(
        serde_json::to_value(ClientMessage::Ack { request_id: 7.into() }).unwrap(),
        json!({"_tag":"Ack","requestId":"7"})
    );
    assert_eq!(serde_json::to_value(ClientMessage::Ping).unwrap(), json!({"_tag":"Ping"}));
}

#[test]
fn decodes_single_and_batched_frames() {
    let single = decode_server_frame(r#"{"_tag":"Chunk","requestId":"1","values":[{"kind":"synchronized"}]}"#).unwrap();
    assert!(matches!(&single[..], [ServerMessage::Chunk { values, .. }] if values.len() == 1));

    let batch = decode_server_frame(r#"[{"_tag":"Pong"},{"_tag":"Exit","requestId":2,"exit":{"_tag":"Success","value":{"sequence":3}}}]"#).unwrap();
    assert!(matches!(batch[0], ServerMessage::Pong));
    let ServerMessage::Exit { request_id, exit } = batch[1].clone() else { panic!() };
    assert_eq!(request_id, RequestId::Num(2));
    assert_eq!(exit.into_result().unwrap(), json!({"sequence":3}));
}

#[test]
fn failure_exit_surfaces_tagged_error() {
    let frame = r#"{"_tag":"Exit","requestId":"4","exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"EnvironmentAuthorizationError","message":"Missing scope"}}]}}"#;
    let ServerMessage::Exit { exit, .. } = decode_server_frame(frame).unwrap().remove(0) else { panic!() };
    let failure = exit.into_result().unwrap_err();
    assert_eq!(failure.tag.as_deref(), Some("EnvironmentAuthorizationError"));
    assert_eq!(failure.message, "Missing scope");
}

#[test]
fn unknown_server_tags_do_not_fail_the_frame() {
    let frames = decode_server_frame(r#"[{"_tag":"SomethingNew","x":1},{"_tag":"Pong"}]"#).unwrap();
    assert!(matches!(frames[0], ServerMessage::Unknown));
    assert!(matches!(frames[1], ServerMessage::Pong));
}
