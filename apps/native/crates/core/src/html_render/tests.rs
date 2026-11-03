use serde_json::{Value, json};

use super::*;

fn reference(height: u32, heights: &[(u32, u32)]) -> HtmlRenderReference {
    HtmlRenderReference { attachment_id: "thread-abc-123.html".into(), title: "Chart".into(), height, heights: heights.to_vec() }
}

/// `htmlRender.test.ts` "reads the reference from each provider's result envelope".
#[test]
fn reads_the_reference_from_each_provider_envelope() {
    let result = json!({ "htmlRender": { "attachmentId": "thread-abc-123.html", "title": "Chart", "height": 420 }, "message": "Rendered above your reply." });
    let text = result.to_string();
    for (tool, output) in [
        ("mcp__t3-code__html_render", json!([{ "type": "text", "text": text }])),
        ("t3-code.html_render", json!({ "structuredContent": result, "content": [] })),
        ("t3-code-thread_1_html_render", Value::String(text.clone())),
        ("html_render", result.clone()),
        // The server's wire projection sends the compacted output itself.
        ("mcp__t3_code__html_render", json!({ "htmlRender": result["htmlRender"] })),
    ] {
        assert_eq!(from_tool_item(Some(tool), Some(&output)), Some(reference(420, &[])), "{tool}");
    }
}

#[test]
fn other_tools_and_failed_results_carry_no_page() {
    let page = json!({ "htmlRender": { "attachmentId": "a.html", "title": "Chart", "height": 420 } });
    assert_eq!(from_tool_item(Some("mcp__other__html_render"), Some(&page)), None);
    assert_eq!(from_tool_item(Some("html_preview"), Some(&page)), None);
    assert_eq!(from_tool_item(None, Some(&page)), None);
    assert_eq!(from_tool_item(Some("html_render"), None), None);
    let failed = json!({ "isError": true, "content": [{ "type": "text", "text": page.to_string() }] });
    assert_eq!(from_tool_item(Some("html_render"), Some(&failed)), None);
    assert_eq!(from_tool_item(Some("html_render"), Some(&json!({ "error": "boom", "htmlRender": page["htmlRender"] }))), None);
    // Past the parse budget, nothing is read.
    let padded = json!({ "htmlRender": page["htmlRender"], "pad": "x".repeat(20_000) }).to_string();
    assert_eq!(from_tool_item(Some("html_render"), Some(&Value::String(padded))), None);
}

/// `resolveT3McpToolId`'s prefixes and the `complete` suffix ACP titles carry.
#[test]
fn resolves_html_render_tool_names() {
    for name in [
        "html_render",
        "mcp__t3-code__html_render",
        "MCP__T3CODE__html_render",
        "mcp__t3_code_direct__html_render",
        "t3-code.html_render",
        "t3_code:html_render",
        "t3code · html_render",
        "t3-code-direct/html_render",
        "t3-code-html_render",
        "mcp-t3-code-html_render",
        "t3 code html_render",
        "t3_code__html_render",
        "t3-code-thread_with_underscores_html_render",
        "html_render completed",
        "mcp__t3-code__html_render Complete",
    ] {
        assert!(is_html_render_tool(name), "{name}");
    }
    for name in ["html_preview", "mcp__github__html_render", "t3-code.html_preview", "my_html_render", "html_renderer", "t3-code.html_render\nx", "Html_render"] {
        assert!(!is_html_render_tool(name), "{name}");
    }
}

#[test]
fn reads_references_and_their_measured_heights() {
    let value = json!({ "attachmentId": "a.html", "title": "  Dashboard  ", "height": 5000.4, "heights": [[860, 300], [320, 900.6], [640, 40]] });
    assert_eq!(
        read_reference(&value),
        Some(HtmlRenderReference { attachment_id: "a.html".into(), title: "Dashboard".into(), height: MAX_HEIGHT, heights: vec![(320, 901), (640, 80), (860, 300)] })
    );
    // One malformed measurement drops them all; a blank title falls back.
    let partial = json!({ "attachmentId": "a.html", "title": " ", "height": 12, "heights": [[320, 900], [0, 10]] });
    assert_eq!(read_reference(&partial), Some(HtmlRenderReference { attachment_id: "a.html".into(), title: "HTML".into(), height: MIN_HEIGHT, heights: Vec::new() }));
    let long = "é".repeat(300);
    assert_eq!(read_reference(&json!({ "attachmentId": "a", "title": long, "height": 100 })).map(|r| r.title.chars().count()), Some(200));
    assert_eq!(read_reference(&json!({ "attachmentId": "", "title": "x", "height": 100 })), None);
    assert_eq!(read_reference(&json!({ "attachmentId": "a".repeat(257), "title": "x", "height": 100 })), None);
    assert_eq!(read_reference(&json!({ "attachmentId": "a", "title": "x", "height": "100" })), None);
}

#[test]
fn frames_fit_the_measured_page_at_their_width() {
    // No measurements: the agent's height, clamped.
    assert_eq!(frame_height(&reference(420, &[]), 500., None), 420);
    assert_eq!(frame_height(&reference(80, &[]), 500., None), MIN_HEIGHT);
    let heights = [(320, 900), (640, 500), (728, 400), (1000, 300)];
    // The taller neighbour, since a breakpoint between them can pick either.
    assert_eq!(frame_height(&reference(400, &heights), 500., None), 900);
    assert_eq!(frame_height(&reference(400, &heights), 640., None), 500);
    // Past the widest measurement, the last two still bound it.
    assert_eq!(frame_height(&reference(400, &heights), 1200., None), 400);
    assert_eq!(frame_height(&reference(400, &heights), 200., None), 900);
    // Taller at the column width than the agent's height: the agent's height caps the frame.
    assert_eq!(frame_height(&reference(350, &heights), 500., None), 350);
    // The live page wins, up to the fit cap.
    assert_eq!(frame_height(&reference(400, &heights), 500., Some(612.2)), 613);
    assert_eq!(frame_height(&reference(400, &heights), 500., Some(10.)), MIN_HEIGHT);
    assert_eq!(frame_height(&reference(400, &heights), 500., Some(90_000.)), MAX_FIT_HEIGHT);
}

#[test]
fn file_names_drop_path_characters() {
    assert_eq!(file_name("Q3 / revenue: \"final\"?"), "Q3 revenue final.html");
    assert_eq!(file_name("  tabs\tand\nlines  "), "tabs and lines.html");
    assert_eq!(file_name("<>|*"), "Page.html");
    assert_eq!(file_name(&"a".repeat(150)), format!("{}.html", "a".repeat(120)));
}

#[test]
fn reads_the_page_height_notification() {
    assert_eq!(read_content_height(&content_height_message(512.)), Some(512.));
    assert_eq!(read_content_height(&json!({ "jsonrpc": "2.0", "method": SIZE_CHANGED_METHOD, "params": { "height": 0 } })), None);
    assert_eq!(read_content_height(&json!({ "jsonrpc": "2.0", "method": "ui/open-link", "params": { "height": 10 } })), None);
    assert_eq!(read_content_height(&json!({ "method": SIZE_CHANGED_METHOD, "params": { "height": 10 } })), None);
}

#[test]
fn pages_load_inline_by_their_file_name() {
    assert_eq!(
        reference(420, &[]).resource().create_url_request(),
        json!({ "resource": { "_tag": "attachment", "attachmentId": "thread-abc-123.html", "fileName": "Chart.html", "mimeType": "text/html", "disposition": "inline" } })
    );
}
