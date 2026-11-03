//! Agent HTML shown in the transcript: the reference an `html_render` tool result carries, the
//! frame's height and the page's file name. Ports `packages/shared/src/htmlRender.ts`
//! (`readHtmlRenderReference`, `htmlRenderFrameHeight`, `htmlRenderFileName`,
//! `readHtmlRenderContentHeight`), the parts of `packages/shared/src/toolOutput.ts`
//! (`compactDynamicToolOutput`, `htmlRenderFromToolItem`) that find a reference in a tool result,
//! and `resolveT3McpToolId` (`t3McpToolPresentation.ts`) narrowed to `html_render`.

use serde_json::{Value, json};

use crate::json_text::{is_js_whitespace, js_trim};
use crate::media::AssetResource;

#[cfg(test)]
mod tests;
pub mod host;
pub mod routing;

pub const TOOL_NAME: &str = "html_render";
pub const MIN_HEIGHT: u32 = 80;
pub const MAX_HEIGHT: u32 = 2000;
/// A frame fits the live page up to this height; it only stops a page that grows with its frame.
pub const MAX_FIT_HEIGHT: u32 = 20_000;
pub const MAX_TITLE_LENGTH: usize = 200;
/// The reply width pages are measured at (`HTML_RENDER_COLUMN_WIDTH`).
pub const COLUMN_WIDTH: f32 = 728.;
/// The bootstrap's `size-changed` notification.
pub const SIZE_CHANGED_METHOD: &str = "ui/notifications/size-changed";

const MAX_MEASURED_HEIGHTS: usize = 24;
const MAX_PARSED_BYTES: usize = 16_384;
const MAX_CONTENT_BLOCKS: usize = 32;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: usize = 128;
const MAX_FILE_STEM: usize = 120;
const T3_MCP_SERVER_ALIASES: [&str; 5] = ["t3-code", "t3_code", "t3code", "t3-code-direct", "t3_code_direct"];

/// `HtmlRenderReference`. `heights` is empty when the server measured none; equality is
/// `htmlRenderReferencesEqual`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlRenderReference {
    pub attachment_id: String,
    pub title: String,
    pub height: u32,
    /// `[width, height]` pairs, sorted by width.
    pub heights: Vec<(u32, u32)>,
}

impl HtmlRenderReference {
    pub fn file_name(&self) -> String {
        file_name(&self.title)
    }

    /// The page as `assets.createUrl` serves it inline (`HtmlRenderFrame.tsx`).
    pub fn resource(&self) -> AssetResource {
        AssetResource::InlineAttachment { attachment_id: self.attachment_id.clone(), file_name: self.file_name(), mime_type: "text/html".into() }
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The longest prefix within `max` UTF-16 units, never splitting a character.
fn utf16_prefix(text: &str, max: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &text[..index];
        }
    }
    text
}

/// `clampHtmlRenderHeight`.
pub fn clamp_height(height: f64) -> u32 {
    height.round().clamp(f64::from(MIN_HEIGHT), f64::from(MAX_HEIGHT)) as u32
}

fn integer(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.fract() == 0.)
}

/// `readMeasuredHeights`: every entry valid, or none at all.
fn read_measured_heights(value: Option<&Value>) -> Vec<(u32, u32)> {
    let Some(entries) = value.and_then(Value::as_array).filter(|entries| !entries.is_empty() && entries.len() <= MAX_MEASURED_HEIGHTS) else {
        return Vec::new();
    };
    let heights: Option<Vec<(u32, u32)>> = entries
        .iter()
        .map(|entry| {
            let pair = entry.as_array().filter(|pair| pair.len() == 2)?;
            let width = integer(&pair[0]).filter(|width| (1. ..=10_000.).contains(width))?;
            Some((width as u32, clamp_height(pair[1].as_f64()?)))
        })
        .collect();
    let mut heights = heights.unwrap_or_default();
    heights.sort_by_key(|(width, _)| *width);
    heights
}

/// `readHtmlRenderReference`.
pub fn read_reference(value: &Value) -> Option<HtmlRenderReference> {
    let object = value.as_object()?;
    let attachment_id = object.get("attachmentId")?.as_str().filter(|id| !id.is_empty() && utf16_len(id) <= 256)?;
    let title = object.get("title")?.as_str()?;
    let height = object.get("height")?.as_f64()?;
    let title = utf16_prefix(js_trim(title), MAX_TITLE_LENGTH);
    Some(HtmlRenderReference {
        attachment_id: attachment_id.to_owned(),
        title: if title.is_empty() { "HTML".to_owned() } else { title.to_owned() },
        height: clamp_height(height),
        heights: read_measured_heights(object.get("heights")),
    })
}

/// `measuredHeight`: the taller neighbouring measurement, since breakpoints can pick either.
fn measured_height(heights: &[(u32, u32)], width: f32) -> u32 {
    let high = heights.iter().position(|(measured, _)| *measured as f32 >= width).unwrap_or(heights.len() - 1);
    let low = if heights[high].0 as f32 == width { high } else { high.saturating_sub(1) };
    heights[low].1.max(heights[high].1)
}

/// `htmlRenderFrameHeight`: a page even a few pixels taller than its frame scrolls inside it and
/// takes the reader's scroll, so the frame fits the page. The live page's `content_height` wins
/// over the agent's measurement, since client fonts often wrap it taller.
pub fn frame_height(reference: &HtmlRenderReference, width: f32, content_height: Option<f32>) -> u32 {
    if let Some(content) = content_height {
        return (content.ceil().max(MIN_HEIGHT as f32) as u32).min(MAX_FIT_HEIGHT);
    }
    if reference.heights.is_empty() {
        return reference.height.clamp(MIN_HEIGHT, MAX_HEIGHT);
    }
    let cap = if measured_height(&reference.heights, COLUMN_WIDTH) > reference.height { reference.height } else { MAX_HEIGHT };
    cap.min(measured_height(&reference.heights, width)).clamp(MIN_HEIGHT, MAX_HEIGHT)
}

/// `htmlRenderFileName`.
pub fn file_name(title: &str) -> String {
    let mut spaced = String::with_capacity(title.len());
    let mut in_run = false;
    for c in title.chars() {
        let replaced = matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control();
        let space = replaced || is_js_whitespace(c);
        if space {
            if !in_run {
                spaced.push(' ');
            }
        } else {
            spaced.push(c);
        }
        in_run = space;
    }
    let name = js_trim(utf16_prefix(js_trim(&spaced), MAX_FILE_STEM));
    format!("{}.html", if name.is_empty() { "Page" } else { name })
}

/// `readHtmlRenderContentHeight`: the page's `size-changed` notification.
pub fn read_content_height(message: &Value) -> Option<f64> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || message.get("method").and_then(Value::as_str) != Some(SIZE_CHANGED_METHOD) {
        return None;
    }
    message.get("params")?.get("height")?.as_f64().filter(|height| height.is_finite() && *height > 0.)
}

/// The notification a host posts to report a page's height, as the bootstrap does.
pub fn content_height_message(height: f64) -> Value {
    json!({ "jsonrpc": "2.0", "method": SIZE_CHANGED_METHOD, "params": { "height": height } })
}

struct Budget {
    bytes: usize,
    nodes: usize,
    exceeded: bool,
}

/// `Predicate.isObject`: arrays count.
fn is_object(value: &Value) -> bool {
    value.is_object() || value.is_array()
}

/// `readResult`: the envelope's data object and whether it reports a failure.
fn read_result(value: &Value, budget: &mut Budget, depth: usize) -> (Option<Value>, bool) {
    if depth > MAX_ENVELOPE_DEPTH || budget.nodes == 0 {
        budget.exceeded = true;
        return (None, false);
    }
    budget.nodes -= 1;
    match value {
        Value::String(text) => {
            if text.len() > budget.bytes {
                budget.exceeded = true;
                return (None, false);
            }
            budget.bytes -= text.len();
            match serde_json::from_str::<Value>(text) {
                Ok(parsed) => read_result(&parsed, budget, depth + 1),
                Err(_) => (None, false),
            }
        }
        Value::Array(blocks) => {
            if blocks.len() > MAX_CONTENT_BLOCKS {
                budget.exceeded = true;
                return (None, false);
            }
            let (mut data, mut failed) = (None, false);
            for block in blocks {
                let text = block.as_object().and_then(|block| block.get("text")).unwrap_or(&Value::Null);
                let inner = if is_object(text) { text.get("text").filter(|inner| !inner.is_null()).unwrap_or(text) } else { text };
                let (block_data, block_failed) = read_result(inner, budget, depth + 1);
                if data.is_none() {
                    data = block_data;
                }
                failed |= block_failed;
                if budget.exceeded {
                    break;
                }
            }
            (data, failed)
        }
        Value::Object(object) => {
            let failed = object.get("isError") == Some(&Value::Bool(true))
                || object.get("is_error") == Some(&Value::Bool(true))
                || object.get("_tag").and_then(Value::as_str) == Some("OrchestratorMcpFailure")
                || object.get("error").is_some_and(|error| !error.is_null());
            let content = object.get("structuredContent").filter(|content| !content.is_null()).or_else(|| object.get("content"));
            match content {
                Some(content) => {
                    let (data, nested_failed) = read_result(content, budget, depth + 1);
                    (data, failed || nested_failed)
                }
                None => (Some(value.clone()), failed),
            }
        }
        _ => (None, false),
    }
}

/// The bounded envelope reader shared with MCP app reference extraction.
pub(crate) fn compact_data(value: &Value) -> Option<Value> {
    let mut budget = Budget { bytes: MAX_PARSED_BYTES, nodes: MAX_ENVELOPE_NODES, exceeded: false };
    let (data, _) = read_result(value, &mut budget, 0);
    if budget.exceeded { None } else { data }
}

/// The trailing `" complete"` some ACP providers append to a tool's title.
fn normalize_tool_label(value: &str) -> &str {
    let trimmed = value.trim_end_matches(is_js_whitespace);
    for suffix in ["completed", "complete"] {
        let Some(at) = trimmed.len().checked_sub(suffix.len()) else { continue };
        if trimmed.is_char_boundary(at) && trimmed[at..].eq_ignore_ascii_case(suffix) {
            let base = &trimmed[..at];
            if base.ends_with(is_js_whitespace) {
                return js_trim(base);
            }
        }
    }
    js_trim(value)
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// What a regex `.+` matches: one or more characters, none a line terminator.
fn dots(text: &str) -> bool {
    !text.is_empty() && !text.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// `/^mcp__(?<server>.+?)__(?<tool>.+)$/i`.
fn mcp_tool(label: &str) -> Option<(&str, &str)> {
    let rest = strip_prefix_ci(label, "mcp__")?;
    (1..rest.len()).find_map(|at| {
        let (server, tail) = rest.split_at_checked(at)?;
        let tool = tail.strip_prefix("__")?;
        (dots(server) && dots(tool)).then_some((server, tool))
    })
}

/// `/^(?<server>t3[-_]code[-_]direct|t3-code|t3_code|t3code)(?:[.:/]|\s*·\s*)(?<tool>.+)$/i`.
fn namespaced_tool(label: &str) -> Option<&str> {
    const SERVERS: [&str; 7] = ["t3-code-direct", "t3-code_direct", "t3_code-direct", "t3_code_direct", "t3-code", "t3_code", "t3code"];
    SERVERS.iter().find_map(|server| {
        let rest = strip_prefix_ci(label, server)?;
        let tool = match rest.strip_prefix(['.', ':', '/']) {
            Some(tool) => tool,
            None => rest.trim_start_matches(is_js_whitespace).strip_prefix('·')?.trim_start_matches(is_js_whitespace),
        };
        dots(tool).then_some(tool)
    })
}

/// `/^(?:mcp[-_]{1,2})?t3[-_ ]?code(?:__|[-_.:/ ])(?<tool>.+)$/i`, tried in the regex's order.
fn prefixed_tool(label: &str) -> Option<&str> {
    let mut starts: Vec<&str> = Vec::new();
    if let Some(rest) = strip_prefix_ci(label, "mcp") {
        let joiner = |text: &str| text.starts_with(['-', '_']);
        if joiner(rest) && joiner(&rest[1..]) {
            starts.push(&rest[2..]);
        }
        if joiner(rest) {
            starts.push(&rest[1..]);
        }
    }
    starts.push(label);
    starts.into_iter().find_map(|start| {
        let rest = strip_prefix_ci(start, "t3")?;
        let mut afters = Vec::new();
        if rest.starts_with(['-', '_', ' ']) {
            afters.push(&rest[1..]);
        }
        afters.push(rest);
        afters.into_iter().find_map(|after| {
            let rest = strip_prefix_ci(after, "code")?;
            let tool = rest.strip_prefix("__").filter(|tool| dots(tool)).or_else(|| rest.strip_prefix(['-', '_', '.', ':', '/', ' ']))?;
            dots(tool).then_some(tool)
        })
    })
}

/// `resolveT3McpToolId(name) === "html_render"`. No other known tool name ends in `html_render`,
/// so the longest-suffix rule needs only this one.
pub fn is_html_render_tool(name: &str) -> bool {
    let label = normalize_tool_label(name);
    if let Some((server, tool)) = mcp_tool(label) {
        return T3_MCP_SERVER_ALIASES.contains(&server.to_ascii_lowercase().as_str()) && tool == TOOL_NAME;
    }
    if let Some(tool) = namespaced_tool(label) {
        return tool == TOOL_NAME;
    }
    let candidate = prefixed_tool(label).unwrap_or(label);
    candidate == TOOL_NAME || (strip_prefix_ci(label, "t3-code-").is_some() && label.ends_with("_html_render"))
}

/// `htmlRenderFromToolItem` for a completed `dynamic_tool` item: the reference in an
/// `html_render` result that did not fail.
pub fn from_tool_item(tool_name: Option<&str>, output: Option<&Value>) -> Option<HtmlRenderReference> {
    if !tool_name.is_some_and(is_html_render_tool) {
        return None;
    }
    let mut budget = Budget { bytes: MAX_PARSED_BYTES, nodes: MAX_ENVELOPE_NODES, exceeded: false };
    let (data, failed) = read_result(output.unwrap_or(&Value::Null), &mut budget, 0);
    if failed || budget.exceeded {
        return None;
    }
    read_reference(data?.get("htmlRender")?)
}
