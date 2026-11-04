use serde_json::{Value, json};

use super::*;

fn item(body: Value) -> TurnItem {
    let mut item = json!({
        "id": "item", "threadId": "thread", "runId": null, "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
        "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-10-01T00:00:00Z",
    });
    item.as_object_mut().expect("object").extend(body.as_object().expect("object").clone());
    serde_json::from_value(item).expect("item decodes")
}

fn tool(output: Value) -> TurnItem {
    item(json!({ "type": "dynamic_tool", "toolName": "screenshots", "input": {}, "output": output }))
}

fn tool_input(input: Value) -> ToolCallLines {
    ToolCallLines::args(&input)
}

/// `itemDetail.test.ts`: detail responses keep image markers after the server strips the bytes.
#[test]
fn images_past_the_served_positions_keep_their_markers() {
    for shape in ["array", "content", "structured"] {
        let wrap = |content: Value| match shape {
            "array" => content,
            "structured" => json!({ "content": content, "structuredContent": { "temp": 21 } }),
            _ => json!({ "content": content }),
        };
        let image = json!({ "type": "image", "mimeType": "image/png" });
        let blocks: Vec<Value> = (0..8).map(|_| image.clone()).collect();
        let within = tool(wrap(Value::Array(blocks.clone())));
        assert_eq!(turn_item_output_text(&within).as_deref(), (shape == "structured").then_some("{\n  \"temp\": 21\n}"), "{shape}");
        assert_eq!(turn_item_output_images(&within).len(), 8, "{shape}");
        assert_eq!(
            turn_item_output_images(&within)[3],
            AssetResource::ToolOutputImage { thread_id: "thread".into(), item_id: "item".into(), index: 3 },
            "{shape}"
        );

        let mut overflow = blocks;
        overflow.push(json!({ "type": "text", "text": "Two additional screenshots:" }));
        overflow.push(json!({ "type": "image", "mimeType": "image/png", "sizeBytes": 100 }));
        overflow.push(json!({ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg" } }));
        let overflow = tool(wrap(Value::Array(overflow)));
        assert_eq!(turn_item_output_images(&overflow).len(), 8, "{shape}");
        let heading = if shape == "structured" { "{\n  \"temp\": 21\n}" } else { "Two additional screenshots:" };
        assert_eq!(turn_item_output_text(&overflow), Some(format!("{heading}\n[image]\n[image]")), "{shape}");
    }
}

#[test]
fn only_supported_raster_images_are_served() {
    assert_eq!(read_tool_output_image(&json!({ "type": "image", "mimeType": "IMAGE/PNG" })).as_deref(), Some("image/png"));
    assert_eq!(read_tool_output_image(&json!({ "type": "image", "mimeType": "image/svg+xml" })), None);
    assert_eq!(read_tool_output_image(&json!({ "type": "image", "source": { "type": "url", "media_type": "image/png" } })), None);
    assert_eq!(read_tool_output_image(&json!({ "type": "image", "source": [] })), None);
    let svg = tool(json!([{ "type": "image", "mimeType": "image/svg+xml" }]));
    assert!(turn_item_output_images(&svg).is_empty());
    assert_eq!(turn_item_output_text(&svg).as_deref(), Some("[image]"));
}

#[test]
fn tool_output_reads_content_blocks_and_pretty_prints_json() {
    let text = |output: Value| turn_item_output_text(&tool(output));
    assert_eq!(text(json!({ "content": [{ "type": "text", "text": "{\"a\":1,\"b\":[true]}" }] })).as_deref(), Some("{\n  \"a\": 1,\n  \"b\": [\n    true\n  ]\n}"));
    assert_eq!(text(json!("{\"a\":1}\n\n{\"b\":2}\n")).as_deref(), Some("{\n  \"a\": 1\n}\n\n{\n  \"b\": 2\n}"), "JSON lines");
    assert_eq!(text(json!("{not json}")).as_deref(), Some("{not json}"));
    assert_eq!(text(json!([{ "type": "resource_link", "uri": "file:///a" }, { "type": "resource", "resource": { "uri": "file:///b" } }])).as_deref(), Some("file:///a\nfile:///b"));
    assert_eq!(text(json!({ "content": [{ "type": "text", "text": "ok" }], "isError": false })).as_deref(), Some("ok"));
    assert_eq!(text(json!({ "content": [{ "type": "text", "text": "done" }], "structuredContent": { "x": 1 } })).as_deref(), Some("done"));
    for content in [json!([]), json!([{ "type": "text", "text": " \u{feff}" }]), Value::Null, json!([{ "unexpected": true }])] {
        assert_eq!(text(json!({ "content": content, "structuredContent": { "x": 1 } })).as_deref(), Some("{\n  \"x\": 1\n}"), "blank or unreadable content: {content}");
    }
    assert_eq!(text(json!({ "rows": 3 })).as_deref(), Some("{\n  \"rows\": 3\n}"));
    assert_eq!(text(json!({})), None);
    assert_eq!(text(json!([])), None);
    assert_eq!(text(json!("   ")), None);
    assert_eq!(text(json!(42)).as_deref(), Some("42"));
    let viewed = item(json!({ "type": "dynamic_tool", "toolName": "view_image", "input": {}, "viewedImagePath": "/a.png", "output": { "type": "image", "mimeType": "image/png" } }));
    assert_eq!(turn_item_output_text(&viewed), None, "a viewed image shows as the preview");
    let omitted = item(json!({ "type": "dynamic_tool", "toolName": "x", "input": {}, "output": "kept", "outputOmitted": true }));
    assert_eq!(turn_item_output_text(&omitted), None);
}

#[test]
fn structured_output_keeps_image_identity_and_uses_json_without_unwrapping_it() {
    let png = json!({ "type": "image", "mimeType": "image/png" });
    let unsupported = json!({ "type": "image", "source": { "type": "url", "media_type": "image/png" } });
    for structured in [json!({ "temp": 21 }), json!({ "type": "text", "text": "Sunny", "temp": 21 }), json!({ "content": [{ "type": "text", "text": "Sunny" }], "temp": 21 })] {
        let item = tool(json!({ "content": [unsupported, png, { "type": "text", "text": "summary" }, png], "structuredContent": structured, "isError": true }));
        assert_eq!(turn_item_output_text(&item), Some(format!("{}\n[image]", json_text::stringify_pretty(&structured))));
        assert_eq!(turn_item_output_images(&item), vec![
            AssetResource::ToolOutputImage { thread_id: "thread".into(), item_id: "item".into(), index: 0 },
            AssetResource::ToolOutputImage { thread_id: "thread".into(), item_id: "item".into(), index: 1 },
        ]);
    }
    for structured in [Value::Null, json!({}), json!([]), json!(" \u{feff}")] {
        let output = tool(json!({ "content": [{ "type": "text", "text": "Sunny" }, png], "structuredContent": structured }));
        assert_eq!(turn_item_output_text(&output).as_deref(), Some("Sunny"), "empty structured value: {structured}");
    }
    for structured in [json!(false), json!(0), json!("Sunny"), json!([1])] {
        let output = tool(json!({ "content": [png], "structuredContent": structured }));
        assert_eq!(turn_item_output_text(&output), Some(json_text::stringify_pretty(&structured)));
    }
    let mut nested = json!({ "content": [], "structuredContent": { "x": 1 } });
    for _ in 0..4 { nested = json!({ "content": nested }); }
    assert_eq!(format_tool_value(Some(&nested), false).as_deref(), Some("{\n  \"x\": 1\n}"), "structured values remain usable at the text depth limit");
    nested = json!({ "content": nested });
    assert_eq!(format_tool_value(Some(&nested), false), Some(json_text::stringify_pretty(&nested)), "past the depth limit the whole value is retained");
    let extra = json!({ "content": [], "structuredContent": { "x": 1 }, "other": true });
    assert_eq!(format_tool_value(Some(&extra), false), Some(json_text::stringify_pretty(&extra)), "unknown envelope fields are retained");
}

#[test]
fn call_lines_show_arguments_in_order_and_other_inputs_as_text() {
    assert_eq!(
        tool_input(json!({ "path": "src/a.rs", "limit": 20, "clear": "", "reset": null, "opts": { "deep": true }, "2": "two" })).args,
        Some(vec![
            ("2".into(), "two".into()),
            ("path".into(), "src/a.rs".into()),
            ("limit".into(), "20".into()),
            ("clear".into(), "\"\"".into()),
            ("reset".into(), "null".into()),
            ("opts".into(), "{\"deep\":true}".into()),
        ])
    );
    assert_eq!(tool_input(json!({})), ToolCallLines::default());
    assert_eq!(tool_input(json!({ "truncated": true, "summary": "big" })).args_text.as_deref(), Some("{\n  \"truncated\": true,\n  \"summary\": \"big\"\n}"));
    assert_eq!(tool_input(json!("[1,2]")).args_text.as_deref(), Some("[\n  1,\n  2\n]"));
    assert_eq!(tool_input(Value::Null), ToolCallLines::default());
    assert_eq!(ToolCallLines::command("  ls -la \n").command.as_deref(), Some("ls -la"));
    assert_eq!(ToolCallLines::command(" ").command, None);
}

#[test]
fn command_output_unwraps_the_claude_envelope() {
    let command = |output: &str| item(json!({ "type": "command_execution", "input": "ls", "output": output }));
    assert_eq!(turn_item_output_text(&command("{\"stdout\":\"out\",\"stderr\":\"err\",\"interrupted\":false}")).as_deref(), Some("out\nerr"));
    assert_eq!(turn_item_output_text(&command("{\"stdout\":\" \",\"stderr\":\"\",\"interrupted\":false}")), None);
    assert_eq!(turn_item_output_text(&command("{\"stdout\":\"out\"}")).as_deref(), Some("{\"stdout\":\"out\"}"));
    assert_eq!(turn_item_output_text(&command("plain\n")).as_deref(), Some("plain\n"));
    assert_eq!(turn_item_output_text(&command("\n ")), None);
}

#[test]
fn search_output_lists_results() {
    let files = item(json!({ "type": "file_search", "pattern": "fn", "results": [{ "fileName": "a.rs", "line": 3, "preview": " fn a() " }, { "fileName": "b.rs" }] }));
    assert_eq!(turn_item_output_text(&files).as_deref(), Some("a.rs:3\nfn a()\nb.rs"));
    let web = item(json!({ "type": "web_search", "results": [{ "title": " Docs ", "url": "https://a.dev", "snippet": "x" }, { "title": "", "url": "https://b.dev" }] }));
    assert_eq!(turn_item_output_text(&web).as_deref(), Some("Docs\nhttps://a.dev\nx\n\nhttps://b.dev"));
}

#[test]
fn items_have_detail_when_their_inspector_shows_something() {
    let has = |body: Value| turn_item_has_detail(&item(body));
    assert!(has(json!({ "type": "command_execution", "input": "ls" })));
    assert!(!has(json!({ "type": "command_execution", "input": " " })));
    assert!(has(json!({ "type": "command_execution", "input": "", "exitCode": 2 })));
    assert!(!has(json!({ "type": "dynamic_tool", "toolName": "x", "input": {} })), "no arguments and no output to fetch");
    assert!(has(json!({ "type": "dynamic_tool", "toolName": "x", "input": {}, "outputOmitted": true })));
    assert!(has(json!({ "type": "dynamic_tool", "toolName": "x", "input": { "a": 1 } })));
    assert!(has(json!({ "type": "file_change", "fileName": "a.rs" })));
    assert!(!has(json!({ "type": "file_search", "pattern": " " })));
    assert!(has(json!({ "type": "web_search", "patterns": ["q"] })));
    assert!(!has(json!({ "type": "notification", "source": {}, "outcome": "completed", "summary": "s" })));
    assert!(has(json!({ "type": "todo_list", "planId": "p", "steps": [{ "id": "1", "text": "a", "status": "pending" }] })));
    assert!(!has(json!({ "type": "compaction", "summary": "s" })));
}

#[test]
fn withheld_output_and_summarized_input_need_a_fetch() {
    assert!(needs_detail_fetch(&item(json!({ "type": "command_execution", "input": "ls", "outputOmitted": true }))));
    assert!(!needs_detail_fetch(&item(json!({ "type": "command_execution", "input": "ls", "output": "a" }))));
    assert!(needs_detail_fetch(&item(json!({ "type": "dynamic_tool", "toolName": "x", "input": { "truncated": true, "summary": "s" } }))));
    assert!(!needs_detail_fetch(&item(json!({ "type": "dynamic_tool", "toolName": "x", "input": { "truncated": true } }))));
}
