//! What an open work row draws under its line: the rules of `WorkEntryLogRow`
//! (`apps/web/src/components/chat/TimelineWorkEntryRow.tsx`: `plainOutput`,
//! `canExpandProjectedItem`, which body opens, `QuestionAnswerHistory`) and the logic of
//! `V2ItemInspector.tsx` (`useFetchedTurnItem`, `ToolOutput`, `StructuredValue`, its text lines).

use serde_json::Value;

use super::detail::{needs_detail_fetch, turn_item_has_detail, turn_item_output_images, turn_item_output_text};
use super::presentation::{ToolGroupAction, format_workspace_relative_path, skill_args, tool_group_action, work_entry_read_output};
use super::user_input::question_answer_text;
use super::WorkLogEntry;
use crate::command_output::{LOADING, NO_OUTPUT};
use crate::json_text::{self, js_trim};
use crate::media::AssetResource;
use crate::turn_item::{FileChangeDetail, PlanStep, TurnItem, TurnItemKind};

/// The error a fetch shows when the server no longer has the item.
pub const OUTPUT_UNAVAILABLE: &str = "Output is no longer available.";
/// `ToolOutput`'s images' alt text.
pub const TOOL_OUTPUT_IMAGE: &str = "Tool output image";
/// A web search result with neither title nor URL.
pub const SEARCH_RESULT: &str = "Search result";

/// Where the fetch of an item's detail stands, for the item's current revision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchedDetail<'a> {
    Pending,
    Loaded(&'a TurnItem),
    /// The server answered without the item.
    Missing,
    Failed(&'a str),
}

/// `ToolOutput`'s state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutput {
    pub output: Option<String>,
    pub images: Vec<AssetResource>,
    pub pending: bool,
    pub error: Option<String>,
    /// The fetch landed and the output is what it brought.
    pub empty: bool,
}

/// The text line under a tool's output images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputLine<'a> {
    Text(&'a str),
    /// "Loading output…", muted and italic.
    Loading,
    /// "Couldn't load output: …", destructive.
    Failed(String),
    /// "No output.", muted and italic.
    Empty,
}

impl OutputLine<'_> {
    pub fn copy(&self) -> String {
        match self {
            OutputLine::Text(text) => (*text).to_owned(),
            OutputLine::Loading => LOADING.to_owned(),
            OutputLine::Failed(error) => format!("Couldn't load output: {error}"),
            OutputLine::Empty => NO_OUTPUT.to_owned(),
        }
    }
}

impl ToolOutput {
    pub fn line(&self) -> Option<OutputLine<'_>> {
        match (&self.output, &self.error) {
            (Some(output), _) if !output.is_empty() => Some(OutputLine::Text(output)),
            _ if self.pending => Some(OutputLine::Loading),
            (_, Some(error)) => Some(OutputLine::Failed(error.clone())),
            _ if self.empty && self.images.is_empty() => Some(OutputLine::Empty),
            _ => None,
        }
    }
}

/// `useFetchedTurnItem`: the fetched item stands in for the wire item once it lands, when it is
/// the same kind of item; until then the output shows how the fetch stands.
pub fn fetched_turn_item<'a>(wire: &'a TurnItem, fetches: bool, detail: Option<FetchedDetail<'a>>) -> (&'a TurnItem, ToolOutput) {
    let shown = |item: &TurnItem| ToolOutput { output: turn_item_output_text(item), images: turn_item_output_images(item), ..ToolOutput::default() };
    if !fetches {
        return (wire, shown(wire));
    }
    match detail.unwrap_or(FetchedDetail::Pending) {
        FetchedDetail::Loaded(fetched) if std::mem::discriminant(&fetched.kind) == std::mem::discriminant(&wire.kind) => {
            (fetched, ToolOutput { empty: true, ..shown(fetched) })
        }
        FetchedDetail::Loaded(_) => (wire, shown(wire)),
        FetchedDetail::Pending => (wire, ToolOutput { pending: true, ..shown(wire) }),
        FetchedDetail::Missing => (wire, ToolOutput { error: Some(OUTPUT_UNAVAILABLE.to_owned()), ..shown(wire) }),
        FetchedDetail::Failed(error) => (wire, ToolOutput { error: Some(error.to_owned()), ..shown(wire) }),
    }
}

/// What an open work row draws under its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryBody {
    /// `QuestionAnswerHistory`.
    QuestionAnswer,
    /// `V2ItemInspector`.
    Inspector,
    /// A read's paths or a skill's arguments as plain text, then `FetchedToolOutput` when the
    /// output was withheld.
    Plain { text: Option<String>, fetches: bool },
}

/// `plainOutput`: reads show their paths and skill calls their arguments, not the inspector.
pub fn plain_output(entry: &WorkLogEntry, workspace_root: Option<&str>) -> Option<Option<String>> {
    if tool_group_action(entry) == ToolGroupAction::Read {
        return Some(work_entry_read_output(entry, workspace_root));
    }
    skill_args(entry)
}

/// `canExpandProjectedItem`, but for viewed images, which rows do not draw yet.
pub fn entry_can_expand(entry: &WorkLogEntry, workspace_root: Option<&str>) -> bool {
    let Some(item) = entry.item.as_deref() else { return false };
    if let Some(text) = plain_output(entry, workspace_root) {
        return text.is_some_and(|text| !text.is_empty()) || entry.question_answer.is_some() || needs_detail_fetch(item);
    }
    if entry.item_type == Some("reasoning") {
        return entry.detail.as_deref().is_some_and(|detail| !js_trim(detail).is_empty());
    }
    entry.question_answer.is_some() || turn_item_has_detail(item)
}

/// The body an open row draws; `None` for reasoning, which draws its trace, and for rows with
/// nothing to show.
pub fn entry_body(entry: &WorkLogEntry, workspace_root: Option<&str>) -> Option<EntryBody> {
    let item = entry.item.as_deref()?;
    if entry.item_type == Some("reasoning") || !entry_can_expand(entry, workspace_root) {
        return None;
    }
    if entry.question_answer.is_some() {
        return Some(EntryBody::QuestionAnswer);
    }
    match plain_output(entry, workspace_root) {
        Some(text) => {
            let text = text.filter(|text| !text.is_empty());
            let fetches = needs_detail_fetch(item);
            (text.is_some() || fetches).then_some(EntryBody::Plain { text, fetches })
        }
        None => Some(EntryBody::Inspector),
    }
}

/// Whether an open row fetches its item's withheld detail. The inspector of a command whose
/// output the server streams (`commandOutputStreaming`) shows the stream instead.
pub fn entry_fetches_detail(entry: &WorkLogEntry, streams_command_output: bool) -> bool {
    let Some(item) = entry.item.as_deref().filter(|item| needs_detail_fetch(item)) else { return false };
    match entry_body(entry, None) {
        Some(EntryBody::Plain { fetches, .. }) => fetches,
        Some(EntryBody::Inspector) => !(streams_command_output && matches!(item.kind, TurnItemKind::CommandExecution { .. })),
        _ => false,
    }
}

/// A user-input request's questions, each with its options as bullets.
pub fn questions_text(questions: &[Value]) -> String {
    questions
        .iter()
        .map(|question| {
            let mut lines = vec![question.get("question").and_then(Value::as_str).unwrap_or_default().to_owned()];
            let options = question.get("options").and_then(Value::as_array).into_iter().flatten();
            lines.extend(options.map(|option| format!("• {}", option.get("label").and_then(Value::as_str).unwrap_or_default())));
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A todo list's steps, marked done, running or pending.
pub fn todo_text(steps: &[PlanStep]) -> String {
    steps
        .iter()
        .map(|step| {
            let mark = match step.status.as_str() {
                "completed" => "✓",
                "running" => "◉",
                _ => "○",
            };
            format!("{mark} {}", step.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One change of a file change: `{operation} {old path → }{path}{ (file type, mime type)}`.
pub fn file_change_line(change: &FileChangeDetail, workspace_root: Option<&str>) -> String {
    let old = change.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
    let kinds: Vec<&str> = [change.file_type.as_deref(), change.mime_type.as_deref()].into_iter().flatten().filter(|kind| !kind.is_empty()).collect();
    let kinds = if kinds.is_empty() { String::new() } else { format!(" ({})", kinds.join(", ")) };
    format!("{} {old}{}{kinds}", change.operation, format_workspace_relative_path(&change.path, workspace_root))
}

/// A file search result's location: `path:line:column`.
pub fn search_result_location(result: &Value, workspace_root: Option<&str>) -> String {
    let file = result.get("fileName").and_then(Value::as_str).unwrap_or_default();
    let at = |key: &str| result.get(key).and_then(Value::as_u64).map(|n| format!(":{n}")).unwrap_or_default();
    format!("{}{}{}", format_workspace_relative_path(file, workspace_root), at("line"), at("column"))
}

/// `resolveExternalWebLinkHref`: an `http(s)` link, protocol-relative ones as `https`.
pub fn external_web_link(href: Option<&str>) -> Option<String> {
    let href = href.filter(|href| !href.is_empty())?;
    let href = if href.starts_with("//") { format!("https:{href}") } else { href.to_owned() };
    let url = url::Url::parse(&href).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// One question of `QuestionAnswerHistory`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredQuestion {
    pub question: Option<String>,
    pub answer: Option<String>,
    pub attachments: Vec<AnswerAttachment>,
}

/// A file given with an answer; images show as thumbnails served by the attachment's asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerAttachment {
    pub id: String,
    pub name: String,
    pub image: bool,
}

/// `QuestionAnswerHistory`: every question asked, answered or given files, in order.
pub fn answered_questions(answer: &Value) -> Vec<AnsweredQuestion> {
    let map = |key: &str| answer.get(key).and_then(Value::as_object);
    let mut ids: Vec<&String> = Vec::new();
    for key in ["questionTextById", "answers", "attachmentsByQuestionId"] {
        for (id, _) in map(key).map(json_text::js_entries).unwrap_or_default() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    let text = |key: &str, id: &str| map(key).and_then(|values| values.get(id));
    ids.into_iter()
        .map(|id| AnsweredQuestion {
            question: text("questionTextById", id).and_then(Value::as_str).filter(|question| !question.is_empty()).map(str::to_owned),
            answer: text("answers", id).map(question_answer_text).filter(|answer| !answer.is_empty()),
            attachments: text("attachmentsByQuestionId", id)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|attachment| {
                    let field = |key: &str| attachment.get(key).and_then(Value::as_str);
                    Some(AnswerAttachment { id: field("id")?.to_owned(), name: field("name").unwrap_or_default().to_owned(), image: field("type") == Some("image") })
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
