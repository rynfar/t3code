use super::*;
use serde_json::json;

fn app() -> Reference {
    read_reference(&json!({"attachmentId":"app", "server":"weather", "tool":"get_weather", "resourceUri":"ui://weather/dashboard", "csp":{"connectDomains":["https://api.weather.test"]}})).unwrap()
}
fn context() -> Value {
    json!({"theme":"dark","displayMode":"inline","availableDisplayModes":["inline"],"containerDimensions":{"width":600,"maxHeight":2000},"platform":"desktop"})
}
fn host() -> Host {
    Host::new(app(), context(), "1.0.0")
}
fn receive(host: &mut Host, id: Value, method: &str, params: Value) -> Vec<Action> {
    host.receive(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
}
fn message(action: &Action) -> &Value {
    let Action::Post(value) = action else { panic!("expected post") };
    value
}

#[test]
fn initializes_then_delivers_input_and_normalized_result_once() {
    let mut h = host();
    assert!(
        h.set_tool_call(
            json!({"city":"Oslo"}),
            Some(json!({"content":[], "_meta":null,"structuredContent":{"time":"t"}}))
        )
        .is_empty()
    );
    let init = receive(&mut h, json!(1), "ui/initialize", json!({"protocolVersion":"old"}));
    assert_eq!(message(&init[0])["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(message(&init[0])["result"]["hostCapabilities"]["sandbox"]["csp"], app().csp);
    assert_eq!(message(&init[0])["result"]["hostCapabilities"]["message"], json!({"text":{}}));
    assert_eq!(
        message(&init[0])["result"]["hostCapabilities"]["updateModelContext"],
        json!({"text":{},"structuredContent":{}})
    );
    assert_eq!(message(&init[0])["result"]["hostCapabilities"]["downloadFile"], json!({}));
    let sent = receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));
    assert_eq!(sent.len(), 2);
    assert_eq!(message(&sent[0])["params"], json!({"arguments":{"city":"Oslo"}}));
    assert_eq!(message(&sent[1])["params"], json!({"content":[],"structuredContent":{"time":"t"}}));
    assert!(receive(&mut h, Value::Null, "ui/notifications/initialized", json!({})).is_empty());
}

#[test]
fn later_result_does_not_repeat_input_or_replace_completed_call() {
    let mut h = host();
    receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));
    assert_eq!(h.set_tool_call(json!({"city":"Oslo"}), None).len(), 1);
    let sent = h.set_tool_call(json!({}), Some(json!({"content":[]})));
    assert_eq!(message(&sent[0])["method"], "ui/notifications/tool-result");
    assert!(h.set_tool_call(json!({}), Some(json!({"content":[1]}))).is_empty());
}

#[test]
fn proxies_tools_resources_links_and_refusals_with_original_ids() {
    let mut h = host();
    for (id, method, params, expected) in [
        (
            json!(7),
            "tools/call",
            json!({"name":"refresh"}),
            Operation::CallTool {
                name: "refresh".into(),
                arguments: json!({}),
            },
        ),
        (
            json!("read"),
            "resources/read",
            json!({"uri":"ui://x"}),
            Operation::ReadResource { uri: "ui://x".into() },
        ),
        (
            json!(9),
            "ui/open-link",
            json!({"url":"HTTPS://example.com"}),
            Operation::OpenLink {
                url: "HTTPS://example.com".into(),
            },
        ),
    ] {
        let sent = receive(&mut h, id.clone(), method, params);
        let Action::Request { token, operation } = &sent[0] else { panic!() };
        assert_eq!(operation, &expected);
        let result = if method == "tools/call" {
            Ok(json!({"content":[],"_meta":null,"isError":false}))
        } else {
            Err("Declined by the user.".into())
        };
        let sent = h.complete(*token, result);
        assert_eq!(message(&sent[0])["id"], id);
        if method == "tools/call" {
            assert_eq!(message(&sent[0])["result"], json!({"content":[]}));
        } else {
            assert_eq!(message(&sent[0])["error"], json!({"code":-32000,"message":"Declined by the user."}));
        }
    }
    assert_eq!(message(&receive(&mut h, json!(1), "ping", json!({}))[0])["result"], json!({}));
}

#[test]
fn rejects_bad_requests_limits_inflight_and_ignores_after_dispose() {
    let mut h = host();
    for (method, params, code) in [
        ("tools/call", json!({"name":"x","arguments":1}), -32602),
        ("resources/read", json!({}), -32602),
        ("ui/open-link", json!({"url":"javascript:x"}), -32602),
        ("sampling/createMessage", json!({}), -32601),
        ("ui/message", json!({}), -32602),
        ("ui/message", json!({"role":"assistant","content":{"type":"text","text":"hi"}}), -32602),
        ("ui/message", json!({"role":"user","content":[{"type":"image","data":"x"}]}), -32602),
        ("ui/message", json!({"role":"user","content":{"type":"text","text":"   "}}), -32602),
        ("ui/update-model-context", json!({"content":"not-array"}), -32602),
        ("ui/update-model-context", json!({"structuredContent":[]}), -32602),
        ("ui/download-file", json!({"contents":[{}]}), -32602),
    ] {
        let sent = receive(&mut h, json!(1), method, params);
        assert_eq!(message(&sent[0])["error"]["code"], code);
    }
    for id in 0..16 {
        assert!(matches!(
            receive(&mut h, json!(id), "tools/call", json!({"name":"slow"}))[0],
            Action::Request { .. }
        ));
    }
    let sent = receive(&mut h, json!(16), "tools/call", json!({"name":"slow"}));
    assert_eq!(message(&sent[0])["error"]["message"], "Too many requests in flight.");
    let large = receive(&mut h, json!(17), "ping", json!({"padding":"x".repeat(MAX_MESSAGE_BYTES)}));
    assert_eq!(message(&large[0])["error"]["code"], -32600);
    for text in ["not-json", "[]", "{\"jsonrpc\":\"1.0\",\"method\":\"ping\",\"id\":1}"] {
        assert!(h.receive(text).is_empty());
    }
    h.dispose();
    assert!(h.complete(0, Ok(json!({}))).is_empty());
    assert!(receive(&mut h, json!(1), "ping", json!({})).is_empty());
}

#[test]
fn catches_up_context_and_clamps_size_notifications() {
    let mut h = host();
    receive(&mut h, json!(1), "ui/initialize", json!({}));
    let mut next = context();
    next["containerDimensions"]["width"] = json!(800);
    assert!(h.update_context(next).is_empty());
    let sent = receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));
    assert_eq!(message(&sent[0])["params"], json!({"containerDimensions":{"width":800,"maxHeight":2000}}));
    for (height, expected) in [(-5., 80), (123.5, 124), (5000., 2000)] {
        assert_eq!(
            receive(&mut h, Value::Null, "ui/notifications/size-changed", json!({"height":height})),
            vec![Action::Height(expected)]
        );
    }
    assert!(receive(&mut h, Value::Null, "ui/notifications/size-changed", json!({"height":"80"})).is_empty());
    assert_eq!(clamp_height(f64::NAN), 320);
}

#[test]
fn inline_close_waits_for_the_matching_teardown_response() {
    for response in [
        json!({"result":{}}),
        json!({"error":{"code":-32000,"message":"Cannot save"}}),
        json!({"error":null}),
    ] {
        let mut h = host();
        receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));
        assert!(h.receive(r#"{"jsonrpc":"2.0","id":"t3-host-0","result":{}}"#).is_empty());
        let pending = receive(&mut h, json!(7), "tools/call", json!({"name":"slow"}));
        let Action::Request { token, .. } = pending[0] else { panic!() };
        let sent = receive(&mut h, Value::Null, "ui/notifications/request-teardown", json!({}));
        assert!(!sent.contains(&Action::Close), "initialized app must finish before close");
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], Action::StartTeardownTimer);
        assert!(h.teardown().is_empty());
        let request = message(&sent[0]);
        assert_eq!(request["method"], "ui/resource-teardown");
        assert_eq!(request["params"], json!({}));
        assert_eq!(request["id"], "t3-host-0");
        assert!(receive(&mut h, Value::Null, "ui/notifications/request-teardown", json!({})).is_empty());
        for invalid in [
            "not-json".to_owned(),
            json!({"jsonrpc":"1.0","id":"t3-host-0","result":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":"t3-host-0"}).to_string(),
            json!({"jsonrpc":"2.0","id":"t3-host-1","result":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":0,"result":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":null,"result":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":"t3-host-0","result":{"padding":"x".repeat(MAX_MESSAGE_BYTES)}}).to_string(),
        ] {
            assert!(h.receive(&invalid).is_empty());
        }
        let ping = receive(&mut h, json!("t3-host-0"), "ping", json!({}));
        assert_eq!(message(&ping[0])["result"], json!({}));
        let mut response = response;
        response["jsonrpc"] = json!("2.0");
        response["id"] = request["id"].clone();
        assert_eq!(h.receive(&response.to_string()), vec![Action::Close]);
        assert!(h.receive(&response.to_string()).is_empty());
        assert!(h.teardown_timeout().is_empty());
        assert!(h.teardown().is_empty());
        assert!(h.complete(token, Ok(json!({"content":[]}))).is_empty());
        assert!(h.set_tool_call(json!({}), Some(json!({"content":[]}))).is_empty());
        assert!(h.update_context(context()).is_empty());
        assert!(receive(&mut h, json!(1), "ping", json!({})).is_empty());
    }
}

#[test]
fn teardown_timeout_and_disposal_are_explicit_and_idempotent() {
    assert_eq!(TEARDOWN_TIMEOUT_MS, 2000);
    for dispose in [false, true] {
        let mut h = host();
        assert!(h.teardown_timeout().is_empty());
        receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));
        let pending = receive(&mut h, json!(7), "resources/read", json!({"uri":"ui://slow"}));
        let Action::Request { token, .. } = pending[0] else { panic!() };
        assert_eq!(h.teardown().len(), 2);
        if dispose {
            h.dispose();
            assert!(h.teardown_timeout().is_empty());
        } else {
            assert_eq!(h.teardown_timeout(), vec![Action::Close]);
        }
        assert!(h.complete(token, Err("late failure".into())).is_empty());
        assert!(h.teardown_timeout().is_empty());
        assert!(h.receive(r#"{"jsonrpc":"2.0","id":"t3-host-0","result":{}}"#).is_empty());
        assert!(h.teardown().is_empty());
    }
}

#[test]
fn uninitialized_close_disposes_without_a_teardown_message() {
    for initialize in [false, true] {
        let mut h = host();
        if initialize {
            receive(&mut h, json!(1), "ui/initialize", json!({}));
        }
        assert_eq!(
            receive(&mut h, Value::Null, "ui/notifications/request-teardown", json!({})),
            vec![Action::Close]
        );
        assert!(receive(&mut h, json!(2), "ping", json!({})).is_empty());
        assert!(receive(&mut h, Value::Null, "ui/notifications/initialized", json!({})).is_empty());
    }
}

#[test]
fn references_match_source_and_keep_policy_narrow() {
    let value = json!({"t3McpApp":{"attachmentId":"app", "server":"weather", "tool":"get_weather", "resourceUri":"ui://weather/dashboard", "csp":{"connectDomains":["https://ok.test","https://bad.test/; script-src *","*"],"resourceDomains":["https://*.cdn.test:443"]}}});
    for output in [
        value.clone(),
        json!({"structuredContent":value}),
        json!([{"type":"text","text":value.to_string()}]),
    ] {
        let app = from_tool_item(Some("weather.get_weather"), Some(&output)).unwrap();
        assert_eq!(app.csp["connectDomains"], json!(["https://ok.test"]));
        assert!(app.content_security_policy().contains("frame-src 'none'"));
        assert_eq!(app.resource()["renderIntent"], "mcp-app");
        assert!(from_tool_item(Some("other.get_weather"), Some(&output)).is_none());
    }
    let mut app = value["t3McpApp"].clone();
    app["resourceUri"] = json!("https://example.com");
    assert!(read_reference(&app).is_none());
}

#[test]
fn parses_permissions_and_forwards_message_context_download_and_display_modes() {
    let reference = read_reference(&json!({
        "attachmentId": "app",
        "server": "weather",
        "tool": "get_weather",
        "resourceUri": "ui://weather/dashboard",
        "permissions": { "camera": {}, "clipboardWrite": {}, "unknown": {}, "microphone": true }
    }))
    .unwrap();
    assert_eq!(reference.permissions, json!({ "camera": {}, "clipboardWrite": {} }));

    let mut ctx = context();
    ctx["availableDisplayModes"] = json!(["inline", "fullscreen"]);
    let mut h = Host::new(reference, ctx.clone(), "1.0.0");
    let init = receive(
        &mut h,
        json!(1),
        "ui/initialize",
        json!({ "appCapabilities": { "availableDisplayModes": ["inline", "fullscreen"] } }),
    );
    assert_eq!(
        message(&init[0])["result"]["hostCapabilities"]["sandbox"]["permissions"],
        json!({ "camera": {}, "clipboardWrite": {} })
    );
    receive(&mut h, Value::Null, "ui/notifications/initialized", json!({}));

    // ui/message accepts one block or an array of blocks
    for (id, content, expected) in [
        (json!(2), json!({"type":"text","text":"one"}), "one"),
        (json!(3), json!([{"type":"text","text":"two"},{"type":"text","text":"three"}]), "two\nthree"),
    ] {
        let sent = receive(&mut h, id.clone(), "ui/message", json!({"role":"user","content":content}));
        let Action::Request { token, operation } = &sent[0] else { panic!() };
        assert_eq!(operation, &Operation::SendMessage { text: expected.into() });
        let done = h.complete(*token, Ok(json!({})));
        assert_eq!(message(&done[0])["id"], id);
        assert_eq!(message(&done[0])["result"], json!({}));
    }

    // ui/update-model-context
    let sent = receive(
        &mut h,
        json!(4),
        "ui/update-model-context",
        json!({"content":[{"type":"text","text":"2 overdue"}],"structuredContent":{"overdue":2}}),
    );
    let Action::Request { token, operation } = &sent[0] else { panic!() };
    assert!(matches!(operation, Operation::UpdateModelContext { content: Some(_), structured_content: Some(_) }));
    assert_eq!(message(&h.complete(*token, Ok(json!({})))[0])["result"], json!({}));

    // ui/download-file (embedded + link + sanitized names + refusal via isError)
    let sent = receive(
        &mut h,
        json!(5),
        "ui/download-file",
        json!({
            "contents": [
                {"type":"resource","resource":{"uri":"file:///re:po?rt.csv","mimeType":"text/csv","text":"a,b"}},
                {"type":"resource_link","uri":"ui://todos/ex*port","name":"bad/name\n.txt"}
            ]
        }),
    );
    let Action::Request { token, operation } = &sent[0] else { panic!() };
    assert_eq!(
        operation,
        &Operation::DownloadFile {
            files: vec![
                DownloadItem::Embedded { name: "rt.csv".into(), mime_type: "text/csv".into(), bytes: b"a,b".to_vec() },
                DownloadItem::Link { name: "bad-name-.txt".into(), uri: "ui://todos/ex*port".into(), mime_type: None },
            ]
        }
    );
    assert_eq!(message(&h.complete(*token, Ok(json!({"isError":true})))[0])["result"], json!({"isError":true}));

    // ui/request-display-mode: pip refused immediately; fullscreen proxied; in fullscreen, size-changed ignored and teardown exits fullscreen
    assert_eq!(
        message(&receive(&mut h, json!(6), "ui/request-display-mode", json!({"mode":"pip"}))[0])["result"],
        json!({"mode":"inline"})
    );
    let sent = receive(&mut h, json!(7), "ui/request-display-mode", json!({"mode":"fullscreen"}));
    let Action::Request { token, operation } = &sent[0] else { panic!() };
    assert_eq!(operation, &Operation::RequestDisplayMode { mode: "fullscreen".into() });
    ctx["displayMode"] = json!("fullscreen");
    let changed = h.update_context(ctx);
    assert_eq!(message(&changed[0])["params"], json!({"displayMode":"fullscreen"}));
    assert_eq!(message(&h.complete(*token, Ok(json!({"mode":"fullscreen"})))[0])["result"], json!({"mode":"fullscreen"}));
    assert!(receive(&mut h, Value::Null, "ui/notifications/size-changed", json!({"height":120})).is_empty());
    assert_eq!(
        receive(&mut h, Value::Null, "ui/notifications/request-teardown", json!({})),
        vec![Action::ExitFullscreen]
    );
}
