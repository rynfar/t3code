//! What an item shows when its row opens, and withheld output (`outputOmitted`) fetched on demand
//! with `orchestration.getTurnItem`: `packages/client-runtime/src/work-log/itemDetail.ts`, with
//! the image predicates of `packages/shared/src/toolOutput.ts`.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use super::presentation::work_entry_is_visible_in_group;
use super::{RowKind, TimelineRow};
use crate::json_text::{self, js_trim};
use crate::media::AssetResource;
use crate::turn_item::{TurnItem, TurnItemKind};
use crate::wire;

pub const GET_TURN_ITEM: &str = "orchestration.getTurnItem";

const MAX_TEXT_BLOCK_DEPTH: usize = 4;
/// `MAX_TOOL_OUTPUT_IMAGES`: the images a tool's output serves, in order.
pub const MAX_TOOL_OUTPUT_IMAGES: usize = 8;
/// `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES`: raster types only.
const IMAGE_MIME_TYPES: [&str; 4] = ["image/gif", "image/jpeg", "image/png", "image/webp"];

fn is_summarized_value(value: &Value) -> bool {
    value.get("truncated") == Some(&Value::Bool(true)) && value.get("summary").is_some_and(Value::is_string)
}

/// `outputBlocks`.
fn output_blocks(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(object) => match object.get("content") {
            Some(Value::Array(content)) => content.iter().collect(),
            _ => vec![value],
        },
        _ => vec![value],
    }
}

/// `readToolOutputImage`: an image block of a supported raster type; its mime type.
pub fn read_tool_output_image(block: &Value) -> Option<String> {
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let source = block.get("source").filter(|source| source.is_object() || source.is_array());
    if source.is_some_and(|source| source.get("type").and_then(Value::as_str) != Some("base64")) {
        return None;
    }
    let mime_type = match source {
        Some(source) => source.get("media_type"),
        None => block.get("mimeType"),
    }?
    .as_str()?
    .to_ascii_lowercase();
    IMAGE_MIME_TYPES.contains(&mime_type.as_str()).then_some(mime_type)
}

/// `toolOutputImageBlocks`: the image blocks served as images, at most eight.
fn tool_output_image_blocks(value: &Value) -> Vec<&Value> {
    output_blocks(value).into_iter().filter(|block| read_tool_output_image(block).is_some()).take(MAX_TOOL_OUTPUT_IMAGES).collect()
}

/// `textFromBlocks`: content blocks as text. An image served as an image takes no text; one past
/// the served ones stays as a marker.
fn text_from_blocks(value: &Value, depth: usize, image_blocks: &[&Value]) -> Option<String> {
    if depth > MAX_TEXT_BLOCK_DEPTH {
        return None;
    }
    let object = match value {
        Value::String(text) => return Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().map(|block| text_from_blocks(block, depth + 1, image_blocks)).collect::<Option<Vec<_>>>()?;
            return Some(parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"));
        }
        Value::Object(object) => object,
        _ => return None,
    };
    let kind = object.get("type").and_then(Value::as_str);
    if kind == Some("text")
        && let Some(text) = object.get("text").and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if kind == Some("image") {
        return Some(if image_blocks.iter().any(|block| std::ptr::eq(*block, value)) { String::new() } else { "[image]".into() });
    }
    if kind == Some("resource_link")
        && let Some(uri) = object.get("uri").and_then(Value::as_str)
    {
        return Some(uri.to_owned());
    }
    if kind == Some("resource")
        && let Some(Value::Object(resource)) = object.get("resource")
        && let Some(text) = resource.get("text").and_then(Value::as_str).or_else(|| resource.get("uri").and_then(Value::as_str))
    {
        return Some(text.to_owned());
    }
    let keys: Vec<&str> = object.keys().map(String::as_str).filter(|key| *key != "isError" && *key != "is_error").collect();
    match keys[..] {
        ["content"] => text_from_blocks(&object["content"], depth + 1, image_blocks),
        [_, _] if keys.contains(&"content") && keys.contains(&"structuredContent") => {
            let text = text_from_blocks(&object["content"], depth + 1, image_blocks);
            let images: Vec<&Value> = object["content"].as_array().into_iter().flatten().filter(|block| block.get("type").and_then(Value::as_str) == Some("image")).collect();
            if let Some(structured) = format_json_value(&object["structuredContent"])
                && (!images.is_empty() || text.as_deref().is_none_or(|text| js_trim(text).is_empty()))
            {
                let markers = (depth < MAX_TEXT_BLOCK_DEPTH)
                    .then(|| images.into_iter().map(|image| text_from_blocks(image, depth + 2, image_blocks)).collect::<Option<Vec<_>>>())
                    .flatten()
                    .map(|parts| parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"));
                return Some([Some(structured), markers.filter(|markers| !markers.is_empty())].into_iter().flatten().collect::<Vec<_>>().join("\n"));
            }
            text.filter(|text| !js_trim(text).is_empty())
        }
        _ => None,
    }
}

/// `formatJsonValue`: empty structured values do not displace content text.
fn format_json_value(value: &Value) -> Option<String> {
    if value.is_null() || value.as_str().is_some_and(|text| js_trim(text).is_empty()) {
        return None;
    }
    let json = json_text::stringify_pretty(value);
    (json != "{}" && json != "[]").then_some(json)
}

/// `prettyJsonText`: a JSON document, or JSON lines, pretty-printed; any other text as is.
fn pretty_json_text(text: &str) -> String {
    let trimmed = js_trim(text);
    if !trimmed.starts_with(['[', '{']) {
        return text.to_owned();
    }
    if let Some(whole) = json_text::parse(trimmed) {
        return json_text::stringify_pretty(&whole);
    }
    let documents: Option<Vec<Value>> = trimmed.split('\n').filter(|line| !js_trim(line).is_empty()).map(|line| json_text::parse(js_trim(line))).collect();
    match documents {
        Some(documents) => documents.iter().map(json_text::stringify_pretty).collect::<Vec<_>>().join("\n\n"),
        None => text.to_owned(),
    }
}

/// `formatToolValue`: text content blocks as text (JSON in them pretty-printed), anything else
/// as pretty JSON; nothing for an empty value, or for a lone image a preview already shows.
pub fn format_tool_value(value: Option<&Value>, has_image_preview: bool) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    if has_image_preview && value.get("type").and_then(Value::as_str) == Some("image") && value.is_object() {
        return None;
    }
    if let Some(text) = text_from_blocks(value, 0, &tool_output_image_blocks(value)) {
        return (!js_trim(&text).is_empty()).then(|| pretty_json_text(&text));
    }
    format_json_value(value)
}

/// `toolCallLines`: a command line, or arguments as `key value` lines, or anything else as text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallLines {
    pub command: Option<String>,
    pub args: Option<Vec<(String, String)>>,
    pub args_text: Option<String>,
}

impl ToolCallLines {
    pub fn command(command: &str) -> Self {
        let command = js_trim(command);
        ToolCallLines { command: (!command.is_empty()).then(|| command.to_owned()), ..Self::default() }
    }

    pub fn args(args: &Value) -> Self {
        match args {
            Value::Object(object) if !is_summarized_value(args) => {
                let entries: Vec<(String, String)> = json_text::js_entries(object)
                    .into_iter()
                    .map(|(key, value)| {
                        // An empty string or null can be the point of a call (a clear or reset), so show it.
                        let text = match value {
                            Value::String(text) if !text.is_empty() => text.clone(),
                            other => json_text::stringify(other),
                        };
                        (key.clone(), text)
                    })
                    .collect();
                ToolCallLines { args: (!entries.is_empty()).then_some(entries), ..Self::default() }
            }
            other => ToolCallLines { args_text: format_tool_value(Some(other), false), ..Self::default() },
        }
    }

    /// Whether `args` gives any line, without building them: rows ask this on every frame.
    fn args_have_lines(args: &Value) -> bool {
        match args {
            Value::Object(object) if !is_summarized_value(args) => !object.is_empty(),
            other => format_tool_value(Some(other), false).is_some(),
        }
    }
}

/// `turnItemNeedsDetailFetch`.
pub fn needs_detail_fetch(item: &TurnItem) -> bool {
    match &item.kind {
        TurnItemKind::CommandExecution { output_omitted, .. } => *output_omitted,
        TurnItemKind::DynamicTool { output_omitted, input, .. } => *output_omitted || is_summarized_value(input),
        _ => false,
    }
}

/// `turnItemDetailRevision`: a running item keeps one key, so an open row fetches once while it
/// streams and again when it finishes.
pub fn detail_revision(item: &TurnItem) -> String {
    match item.status.as_str() {
        "idle" | "pending" | "running" | "waiting" => "live".into(),
        _ => item.updated_at.clone(),
    }
}

pub fn detail_request(item: &TurnItem) -> Value {
    json!({ "threadId": item.thread_id, "itemId": item.id, "revision": detail_revision(item) })
}

/// The fetched item, `None` when the server no longer has it.
pub fn decode_detail(mut value: Value) -> Result<Option<TurnItem>, serde_json::Error> {
    if let Some(ids) = wire::take_id_table(&mut value)
        && let Some(object) = value.as_object_mut()
    {
        wire::expand_record_ids(object, &ids);
    }
    match value.get_mut("item").map(Value::take) {
        None | Some(Value::Null) => Ok(None),
        Some(item) => serde_json::from_value::<TurnItem>(item).map(|item| item.is_known().then_some(item)),
    }
}

/// `commandOutputText`: Claude's `{"stdout","stderr","interrupted"}` envelope as plain text.
pub fn command_output_text(output: &str) -> Option<String> {
    if js_trim(output).is_empty() {
        return None;
    }
    if !output.trim_start_matches(json_text::is_js_whitespace).starts_with("{\"stdout\"") {
        return Some(output.to_owned());
    }
    let envelope = json_text::parse(js_trim(output)).filter(|parsed| {
        parsed.get("stdout").is_some_and(Value::is_string) && parsed.get("stderr").is_some_and(Value::is_string) && parsed.get("interrupted").is_some_and(Value::is_boolean)
    });
    let Some(parsed) = envelope else { return Some(output.to_owned()) };
    let text = ["stdout", "stderr"].iter().filter_map(|key| parsed[*key].as_str()).filter(|part| !js_trim(part).is_empty()).collect::<Vec<_>>().join("\n");
    (!text.is_empty()).then_some(text)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// `turnItemOutputText`.
pub fn turn_item_output_text(item: &TurnItem) -> Option<String> {
    match &item.kind {
        TurnItemKind::CommandExecution { output, .. } => output.as_deref().and_then(command_output_text),
        TurnItemKind::DynamicTool { output_omitted: true, .. } => None,
        TurnItemKind::DynamicTool { output, viewed_image_path, .. } => format_tool_value(output.as_ref(), viewed_image_path.is_some()),
        TurnItemKind::FileSearch { results: Some(results), .. } if !results.is_empty() => Some(
            results
                .iter()
                .map(|result| {
                    let line = result.get("line").and_then(Value::as_u64).map(|line| format!(":{line}")).unwrap_or_default();
                    let location = format!("{}{line}", text(result, "fileName").unwrap_or_default());
                    let preview = text(result, "preview").map(js_trim).filter(|preview| !preview.is_empty());
                    [Some(location.as_str()).filter(|location| !location.is_empty()), preview].into_iter().flatten().collect::<Vec<_>>().join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        TurnItemKind::WebSearch { results: Some(results), .. } if !results.is_empty() => Some(
            results
                .iter()
                .map(|result| {
                    let title = text(result, "title");
                    let url = text(result, "url");
                    let heading = title.map(js_trim).filter(|title| !title.is_empty()).or(url);
                    let link = if title.is_some_and(|title| !title.is_empty()) { url } else { None };
                    let snippet = text(result, "snippet").map(js_trim);
                    [heading, link, snippet].into_iter().flatten().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        _ => None,
    }
}

/// `turnItemOutputImages`: the images a tool returned, as the assets that serve them.
pub fn turn_item_output_images(item: &TurnItem) -> Vec<AssetResource> {
    let TurnItemKind::DynamicTool { output: Some(output), output_omitted: false, .. } = &item.kind else { return Vec::new() };
    (0..tool_output_image_blocks(output).len())
        .map(|index| AssetResource::ToolOutputImage { thread_id: item.thread_id.clone(), item_id: item.id.clone(), index })
        .collect()
}

/// `turnItemHasDetail`: whether the item's inspector has anything to show.
pub fn turn_item_has_detail(item: &TurnItem) -> bool {
    let present = |text: &Option<String>| text.as_deref().is_some_and(|text| !js_trim(text).is_empty());
    match &item.kind {
        TurnItemKind::Reasoning { text, .. } => !js_trim(text).is_empty(),
        TurnItemKind::CommandExecution { input, output, output_omitted, exit_code, .. } => {
            ToolCallLines::command(input).command.is_some() || *output_omitted || present(output) || exit_code.is_some_and(|code| code != 0)
        }
        TurnItemKind::FileChange { .. } | TurnItemKind::Checkpoint { .. } | TurnItemKind::Fork { .. } | TurnItemKind::Handoff { .. } => true,
        TurnItemKind::FileSearch { pattern, results } => results.as_ref().is_some_and(|results| !results.is_empty()) || present(pattern),
        TurnItemKind::WebSearch { patterns, results } => {
            results.as_ref().is_some_and(|results| !results.is_empty()) || patterns.as_ref().is_some_and(|patterns| !patterns.is_empty())
        }
        TurnItemKind::DynamicTool { input, output_omitted, .. } => *output_omitted || ToolCallLines::args_have_lines(input),
        TurnItemKind::ApprovalRequest { prompt, .. } => present(prompt),
        TurnItemKind::UserInputRequest { questions, .. } => !questions.is_empty(),
        TurnItemKind::Notification { detail, .. } => present(detail),
        TurnItemKind::SystemNotice { message } => !js_trim(message).is_empty(),
        TurnItemKind::Error { failure, .. } => !js_trim(&failure.message).is_empty(),
        TurnItemKind::ProposedPlan { markdown, .. } => !js_trim(markdown).is_empty(),
        TurnItemKind::TodoList { steps, .. } => !steps.is_empty(),
        TurnItemKind::Subagent { child_thread_id, .. } => child_thread_id.is_some(),
        _ => false,
    }
}

/// The open command entries the rows draw, with their items: where the web mounts
/// `CommandOutputPanel`. Entries in a folded group or turn are not drawn, so they get none.
pub fn open_command_items(rows: &[Arc<TimelineRow>], expanded: &HashSet<String>) -> Vec<(String, Arc<TurnItem>)> {
    rows.iter()
        .filter_map(|row| match &row.kind {
            RowKind::Work { entries, expanded_group, .. } => Some((entries, *expanded_group)),
            _ => None,
        })
        .flat_map(|(entries, expanded_group)| entries.iter().filter(move |entry| work_entry_is_visible_in_group(entry, expanded_group)))
        .filter(|entry| expanded.contains(&entry.id))
        .filter_map(|entry| {
            let item = entry.item.clone()?;
            matches!(item.kind, TurnItemKind::CommandExecution { .. }).then(|| (entry.id.clone(), item))
        })
        .collect()
}

#[cfg(test)]
mod tests;
