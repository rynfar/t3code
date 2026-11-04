//! Deterministic host transcript for the snapshot weather fixture; no server or provider writes.
use serde_json::json;
use t3_core::mcp_apps::{Action, Host, Operation, read_reference};

fn main() {
    let reference = read_reference(&json!({"attachmentId":"fixture","server":"weather","tool":"get_weather","resourceUri":"ui://weather/dashboard"})).unwrap();
    let mut host = Host::new(
        reference,
        json!({"theme":"dark","platform":"desktop","displayMode":"inline","availableDisplayModes":["inline"],"containerDimensions":{"width":728,"maxHeight":2000}}),
        "fixture",
    );
    host.set_tool_call(json!({"city":"Oslo"}), Some(json!({"content":[{"type":"text","text":"Sunny"}],"_meta":null})));
    for message in [
        json!({"jsonrpc":"2.0","id":1,"method":"ui/initialize","params":{"protocolVersion":"2026-01-26"}}),
        json!({"jsonrpc":"2.0","method":"ui/notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"ui/notifications/size-changed","params":{"height":236}}),
        json!({"jsonrpc":"2.0","id":2,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_weather","arguments":{"city":"Oslo"}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"ui://weather/dashboard"}}),
        json!({"jsonrpc":"2.0","id":5,"method":"ui/open-link","params":{"url":"javascript:x"}}),
        json!({"jsonrpc":"2.0","id":6,"method":"unknown-method"}),
    ] {
        println!("app -> {message}");
        for action in host.receive(&message.to_string()) {
            match action {
                Action::Post(value) => println!("host -> {value}"),
                Action::Height(height) => println!("frame height -> {height}"),
                Action::Request { token, operation } => {
                    println!("network boundary -> {operation:?}");
                    let result = match operation {
                        Operation::CallTool { .. } => json!({"content":[],"structuredContent":{"weather":"Sunny"},"_meta":null}),
                        Operation::ReadResource { .. } => json!({"contents":[{"uri":"ui://weather/dashboard","text":"fixture"}]}),
                        Operation::OpenLink { .. } => json!({}),
                    };
                    for action in host.complete(token, Ok(result)) {
                        if let Action::Post(value) = action {
                            println!("host -> {value}");
                        }
                    }
                }
                Action::Close => println!("closed"),
                Action::StartTeardownTimer => println!("start one cancellable 2000ms teardown deadline"),
            }
        }
    }
    for (label, response) in [
        ("result", Some(json!({"jsonrpc":"2.0","id":"t3-host-0","result":{}}))),
        (
            "error",
            Some(json!({"jsonrpc":"2.0","id":"t3-host-0","error":{"code":-32000,"message":"fixture"}})),
        ),
        ("timeout", None),
    ] {
        let mut host = Host::new(
            read_reference(&json!({"attachmentId":"fixture","server":"weather","tool":"get_weather","resourceUri":"ui://weather/dashboard"})).unwrap(),
            json!({}),
            "fixture",
        );
        host.receive(r#"{"jsonrpc":"2.0","method":"ui/notifications/initialized"}"#);
        println!("teardown scenario -> {label}");
        let actions = host.receive(r#"{"jsonrpc":"2.0","method":"ui/notifications/request-teardown"}"#);
        assert!(!actions.contains(&Action::Close));
        for action in actions {
            println!("host lifecycle -> {action:?}");
        }
        assert!(host.teardown().is_empty());
        assert!(host.receive(r#"{"jsonrpc":"2.0","id":"unrelated","result":{}}"#).is_empty());
        let actions = if let Some(response) = response {
            host.receive(&response.to_string())
        } else {
            host.teardown_timeout()
        };
        assert_eq!(actions, vec![Action::Close]);
        println!("settled -> {actions:?}");
        assert!(host.receive(r#"{"jsonrpc":"2.0","id":"t3-host-0","result":{}}"#).is_empty());
        assert!(host.teardown_timeout().is_empty());
        println!("late acknowledgement/deadline -> ignored");
    }
    let mut host = Host::new(
        read_reference(&json!({"attachmentId":"fixture","server":"weather","tool":"get_weather","resourceUri":"ui://weather/dashboard"})).unwrap(),
        json!({}),
        "fixture",
    );
    assert_eq!(host.teardown(), vec![Action::Close]);
    println!("uninitialized -> Close without any host message");
}
