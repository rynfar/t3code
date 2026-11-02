//! The inspector an open tool row shows: `V2ItemInspector.tsx` (`ToolCallBody`, `StructuredValue`,
//! `ToolOutput`) and `FetchedToolOutput`, over the rules in `t3_core::timeline::inspector`. Its text
//! is selectable, long blocks scroll in their own capped scrollers, JSON arguments are highlighted
//! with the captures the web's highlighter uses, and a tool's images come from the media cache.
//! What it shows is derived once per item, fetched item and appearance, not on every frame.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, HighlightStyle, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::{ActiveTheme, Rope, h_flex, v_flex};
use serde_json::Value;
use t3_core::json_text::{is_json_text, js_trim, json_tokens};
use t3_core::media::MediaSource;
use t3_core::timeline::{
    AnsweredQuestion, FetchedDetail, OutputLine, SEARCH_RESULT, TOOL_OUTPUT_IMAGE, ToolCallLines, external_web_link, fetched_turn_item, file_change_line, format_workspace_relative_path,
    is_javascript_repl_tool, questions_text, search_result_location, todo_text, with_visible_control_characters,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};
use t3_core::vcs::format_diff_count;

use super::media::tool_output_image;
use super::parts::mono_frame;
use crate::icons::Icon;
use crate::selectable_text::{Highlights, SelectableText};
use crate::theme::{hsla, palette, rem};

/// How the fetch of the item's withheld detail stands, as the row's detail cache holds it.
#[derive(Clone)]
pub enum Fetch {
    /// Nothing to fetch.
    None,
    Pending,
    Loaded(Arc<TurnItem>),
    Missing,
    Failed(SharedString),
}

/// What the inspector draws: everything `V2ItemInspector` shows, or only the output
/// (`FetchedToolOutput`, under a read's paths or a skill's arguments).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    OutputOnly,
}

#[derive(IntoElement)]
pub struct ItemInspector {
    id: SharedString,
    item: Arc<TurnItem>,
    fetch: Fetch,
    mode: Mode,
    workspace_root: Option<String>,
    /// A streamed command's output panel, in place of the item's own output.
    output_slot: Option<Option<AnyElement>>,
    /// A file change's "Open diff".
    open_diff: Option<AnyElement>,
}

impl ItemInspector {
    pub fn new(entry_id: &str, item: Arc<TurnItem>, fetch: Fetch, mode: Mode) -> Self {
        ItemInspector {
            id: SharedString::from(format!("inspector:{entry_id}")),
            item,
            fetch,
            mode,
            workspace_root: None,
            output_slot: None,
            open_diff: None,
        }
    }

    pub fn workspace_root(mut self, root: Option<&str>) -> Self {
        self.workspace_root = root.map(str::to_owned);
        self
    }

    pub fn output_slot(mut self, slot: Option<AnyElement>) -> Self {
        self.output_slot = Some(slot);
        self
    }

    pub fn open_diff(mut self, button: Option<AnyElement>) -> Self {
        self.open_diff = button;
        self
    }
}

/// A text with its highlighted runs, for a selectable block.
#[derive(Clone)]
struct Text {
    text: SharedString,
    highlights: Option<Highlights>,
}

impl Text {
    fn plain(text: impl Into<SharedString>) -> Self {
        Text { text: text.into(), highlights: None }
    }
}

/// One piece of the inspector, top to bottom.
enum Block {
    /// `ToolCallBody`: the command, arguments and output, in mono at code size.
    Call(Vec<CallPart>),
    /// `StructuredValue` on its own.
    Structured(Text),
    FileChange { path: SharedString, counts: Option<(SharedString, SharedString)>, failed_diff: Option<Text>, changes: Vec<SharedString> },
    FileSearch { pattern: Option<SharedString>, results: Vec<(SharedString, Option<SharedString>)> },
    WebSearch { patterns: Option<SharedString>, results: Vec<WebResult> },
}

enum CallPart {
    /// `ShellCommandBlock`, not highlighted yet.
    Command(SharedString),
    /// `key value` lines: keys muted, values at 85%.
    Args(Text),
    /// The key of a code argument whose value follows as its own block.
    Key(SharedString),
    /// `StructuredValue` inside the call.
    Structured(Text),
    Output { images: Vec<MediaSource>, line: Option<Line> },
    Exit(SharedString),
}

#[derive(Clone)]
enum Line {
    Text(SharedString),
    Note(SharedString),
    Failed(SharedString),
}

struct WebResult {
    title: SharedString,
    href: Option<String>,
    snippet: Option<SharedString>,
}

/// What the derived blocks depend on. Items compare by identity: a changed item is a new one.
struct Key {
    item: Arc<TurnItem>,
    fetch: Fetch,
    mode: Mode,
    dark: bool,
    streamed: bool,
    workspace_root: Option<String>,
}

impl Key {
    fn same(&self, other: &Key) -> bool {
        let fetch = match (&self.fetch, &other.fetch) {
            (Fetch::None, Fetch::None) | (Fetch::Pending, Fetch::Pending) | (Fetch::Missing, Fetch::Missing) => true,
            (Fetch::Loaded(a), Fetch::Loaded(b)) => Arc::ptr_eq(a, b),
            (Fetch::Failed(a), Fetch::Failed(b)) => a == b,
            _ => false,
        };
        fetch
            && Arc::ptr_eq(&self.item, &other.item)
            && (self.mode, self.dark, self.streamed) == (other.mode, other.dark, other.streamed)
            && self.workspace_root == other.workspace_root
    }
}

#[derive(Default)]
struct Memo {
    key: Option<Key>,
    blocks: Arc<Vec<Block>>,
}

impl RenderOnce for ItemInspector {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = Key {
            item: self.item.clone(),
            fetch: self.fetch.clone(),
            mode: self.mode,
            dark: cx.theme().is_dark(),
            streamed: self.output_slot.is_some(),
            workspace_root: self.workspace_root.clone(),
        };
        let memo = window.use_keyed_state(ElementId::Name(SharedString::from(format!("{}:memo", self.id))), cx, |_, _| Memo::default());
        let blocks = memo.update(cx, |memo, cx| {
            if !memo.key.as_ref().is_some_and(|old| old.same(&key)) {
                memo.blocks = Arc::new(derive_blocks(&self.item, &self.fetch, self.mode, self.workspace_root.as_deref(), self.output_slot.is_some(), cx));
                memo.key = Some(key);
            }
            memo.blocks.clone()
        });
        let ItemInspector { id, mut output_slot, mut open_diff, mode, .. } = self;
        let p = palette(cx);
        let children: Vec<AnyElement> = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| render_block(&format!("{id}:{index}"), block, &mut output_slot, &mut open_diff, cx))
            .collect();
        match mode {
            // `FetchedToolOutput`: `mt-1.5 space-y-1.5`, in mono.
            Mode::OutputOnly => mono_frame(cx).mt(px(6.)).child(v_flex().w_full().gap(px(6.)).children(children)).into_any_element(),
            // `space-y-2 text-xs`.
            Mode::Full => v_flex().w_full().min_w_0().gap(px(8.)).text_xs().text_color(hsla(p.foreground)).children(children).into_any_element(),
        }
    }
}

fn fetched_detail(fetch: &Fetch) -> (bool, Option<FetchedDetail<'_>>) {
    match fetch {
        Fetch::None => (false, None),
        Fetch::Pending => (true, Some(FetchedDetail::Pending)),
        Fetch::Loaded(item) => (true, Some(FetchedDetail::Loaded(item))),
        Fetch::Missing => (true, Some(FetchedDetail::Missing)),
        Fetch::Failed(message) => (true, Some(FetchedDetail::Failed(message))),
    }
}

fn derive_blocks(wire: &TurnItem, fetch: &Fetch, mode: Mode, workspace_root: Option<&str>, streamed: bool, cx: &App) -> Vec<Block> {
    let (fetches, detail) = fetched_detail(fetch);
    let (item, output) = fetched_turn_item(wire, fetches, detail);
    let output_part = || CallPart::Output {
        images: output.images.iter().cloned().map(MediaSource::Asset).collect(),
        line: output.line().map(|line| match line {
            OutputLine::Text(text) => Line::Text(text.to_owned().into()),
            OutputLine::Failed(_) => Line::Failed(line.copy().into()),
            OutputLine::Loading | OutputLine::Empty => Line::Note(line.copy().into()),
        }),
    };
    if mode == Mode::OutputOnly {
        return vec![Block::Call(vec![output_part()])];
    }
    let structured = |value: Option<&str>| value.filter(|text| !text.is_empty()).map(|text| Block::Structured(Text::plain(text.to_owned())));
    match &item.kind {
        TurnItemKind::CommandExecution { input, exit_code, .. } => {
            let mut parts = call_parts(ToolCallLines::command(input), None, cx);
            // The streamed panel stands in for the output; `render_block` puts it here.
            parts.push(if streamed { CallPart::Output { images: Vec::new(), line: None } } else { output_part() });
            parts.extend(exit_code.filter(|code| *code != 0).map(|code| CallPart::Exit(format!("exit {code}").into())));
            vec![Block::Call(parts)]
        }
        TurnItemKind::DynamicTool { tool_name, input, .. } => {
            let mut parts = call_parts(ToolCallLines::args(input), tool_name.as_deref(), cx);
            parts.push(output_part());
            vec![Block::Call(parts)]
        }
        TurnItemKind::FileChange { file_name, additions, deletions, diff_str, changes, .. } => {
            let counts = (additions.is_some() || deletions.is_some())
                .then(|| (format!("+{}", format_diff_count(additions.unwrap_or(0))).into(), format!("-{}", format_diff_count(deletions.unwrap_or(0))).into()));
            let failed_diff = (item.status == "failed").then_some(diff_str.as_deref()).flatten().filter(|diff| !js_trim(diff).is_empty());
            vec![Block::FileChange {
                path: format_workspace_relative_path(file_name, workspace_root).into(),
                counts,
                failed_diff: failed_diff.map(|diff| Text::plain(diff.to_owned())),
                changes: changes.iter().flatten().map(|change| file_change_line(change, workspace_root).into()).collect(),
            }]
        }
        TurnItemKind::FileSearch { pattern, results } => {
            let pattern = pattern.as_deref().filter(|pattern| !js_trim(pattern).is_empty()).map(|pattern| pattern.to_owned().into());
            let results = results
                .iter()
                .flatten()
                .map(|result| {
                    let preview = result.get("preview").and_then(Value::as_str).filter(|preview| !preview.is_empty()).map(|preview| preview.to_owned().into());
                    (search_result_location(result, workspace_root).into(), preview)
                })
                .collect();
            vec![Block::FileSearch { pattern, results }]
        }
        TurnItemKind::WebSearch { patterns, results } => {
            let patterns = patterns.as_ref().filter(|patterns| !patterns.is_empty()).map(|patterns| patterns.join("\n").into());
            let results = results
                .iter()
                .flatten()
                .map(|result| {
                    let field = |key: &str| result.get(key).and_then(Value::as_str);
                    let title = field("title").or(field("url")).unwrap_or(SEARCH_RESULT).to_owned().into();
                    WebResult { title, href: external_web_link(field("url")), snippet: field("snippet").filter(|snippet| !snippet.is_empty()).map(|snippet| snippet.to_owned().into()) }
                })
                .collect();
            vec![Block::WebSearch { patterns, results }]
        }
        TurnItemKind::ApprovalRequest { prompt, .. } => structured(prompt.as_deref()).into_iter().collect(),
        TurnItemKind::UserInputRequest { questions, .. } => structured(Some(&questions_text(questions))).into_iter().collect(),
        TurnItemKind::Notification { detail, .. } => structured(detail.as_deref()).into_iter().collect(),
        TurnItemKind::SystemNotice { message } => structured(Some(message)).into_iter().collect(),
        TurnItemKind::Error { failure, .. } => structured(Some(&failure.message)).into_iter().collect(),
        TurnItemKind::TodoList { steps, .. } => structured(Some(&todo_text(steps))).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// `ToolCallBody`'s command and arguments: a JavaScript REPL's `code` highlighted as TypeScript,
/// anything that is not a record of arguments as highlighted JSON when it parses.
fn call_parts(call: ToolCallLines, tool_name: Option<&str>, cx: &App) -> Vec<CallPart> {
    let mut parts = Vec::new();
    if let Some(command) = call.command {
        parts.push(CallPart::Command(with_visible_control_characters(&command).into()));
    }
    let mut lines = String::new();
    let mut keys: Vec<Range<usize>> = Vec::new();
    let flush = |lines: &mut String, keys: &mut Vec<Range<usize>>, parts: &mut Vec<CallPart>| {
        if !lines.is_empty() {
            parts.push(CallPart::Args(args_text(std::mem::take(lines), std::mem::take(keys), cx)));
        }
    };
    for (key, value) in call.args.into_iter().flatten() {
        if key == "code" && is_javascript_repl_tool(tool_name) {
            flush(&mut lines, &mut keys, &mut parts);
            parts.push(CallPart::Key(key.into()));
            parts.push(CallPart::Structured(typescript(value, cx)));
            continue;
        }
        if !lines.is_empty() {
            lines.push('\n');
        }
        keys.push(lines.len()..lines.len() + key.len());
        lines.push_str(&key);
        lines.push(' ');
        lines.push_str(&value);
    }
    flush(&mut lines, &mut keys, &mut parts);
    if let Some(text) = call.args_text {
        let highlighted = if is_json_text(&text) { json(text, cx) } else { Text::plain(text) };
        parts.push(CallPart::Structured(highlighted));
    }
    parts
}

fn args_text(text: String, keys: Vec<Range<usize>>, cx: &App) -> Text {
    let muted = HighlightStyle { color: Some(hsla(palette(cx).muted_foreground)), ..HighlightStyle::default() };
    Text { text: text.into(), highlights: Some(keys.into_iter().map(|range| (range, muted)).collect()) }
}

/// JSON with each run in its capture's color, as the web's highlighter paints it.
fn json(text: String, cx: &App) -> Text {
    let theme = cx.theme().highlight_theme.clone();
    let highlights: Highlights = json_tokens(&text).into_iter().filter_map(|(range, token)| theme.style(token.capture()).map(|style| (range, style))).collect();
    Text { text: text.into(), highlights: Some(highlights) }
}

fn typescript(code: String, cx: &App) -> Text {
    let theme = cx.theme().highlight_theme.clone();
    let mut highlighter = SyntaxHighlighter::new("typescript");
    highlighter.update(None, &Rope::from_str(&code), None);
    let highlights: Highlights = highlighter.styles(&(0..code.len()), &*theme).into_iter().filter(|(_, style)| *style != HighlightStyle::default()).collect();
    Text { text: code.into(), highlights: Some(highlights) }
}

/// Selectable text in a scroller capped at `max_height`: drags past its edge scroll it.
#[derive(IntoElement)]
struct Capped {
    id: SharedString,
    text: Text,
    max_height: f32,
    color: Hsla,
}

impl RenderOnce for Capped {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window.use_keyed_state(ElementId::Name(SharedString::from(format!("{}:scroll", self.id))), cx, |_, _| ScrollHandle::new()).read(cx).clone();
        let mut text = SelectableText::new(SharedString::from(format!("{}:text", self.id)), self.text.text).autoscroll(&scroll);
        if let Some(highlights) = self.text.highlights {
            text = text.highlights(highlights);
        }
        div().id(self.id).w_full().max_h(rem(self.max_height)).overflow_y_scroll().track_scroll(&scroll).text_color(self.color).child(text)
    }
}

fn selectable(id: &str, text: impl Into<SharedString>) -> SelectableText {
    SelectableText::new(SharedString::from(id.to_owned()), text)
}

fn render_block(id: &str, block: &Block, output_slot: &mut Option<Option<AnyElement>>, open_diff: &mut Option<AnyElement>, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let mono_xs = |cx: &App| div().min_w_0().font_family(cx.theme().mono_font_family.clone()).text_xs();
    match block {
        Block::Call(parts) => {
            let parts: Vec<AnyElement> = parts.iter().enumerate().map(|(index, part)| render_part(&format!("{id}:{index}"), part, output_slot, cx)).collect();
            mono_frame(cx).child(v_flex().w_full().min_w_0().gap(px(6.)).children(parts)).into_any_element()
        }
        Block::Structured(text) => mono_frame(cx).child(structured(id, text, cx)).into_any_element(),
        Block::FileChange { path, counts, failed_diff, changes } => {
            let header = h_flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(mono_xs(cx).text_color(hsla(p.muted_foreground)).child(selectable(&format!("{id}:path"), path.clone())))
                .children(counts.as_ref().map(|(added, removed)| {
                    h_flex().gap(px(4.)).child(div().text_color(hsla(p.diff_addition)).child(added.clone())).child(div().text_color(hsla(p.diff_deletion)).child(removed.clone()))
                }))
                .children(open_diff.take());
            v_flex()
                .w_full()
                .gap(px(8.))
                .child(header)
                .children(failed_diff.as_ref().map(|diff| mono_frame(cx).child(structured(&format!("{id}:diff"), diff, cx))))
                .when_some((!changes.is_empty()).then_some(changes), |this, changes| {
                    this.child(
                        mono_xs(cx).text_color(hsla(p.muted_foreground)).child(v_flex().gap(px(4.)).children(
                            changes.iter().enumerate().map(|(index, change)| selectable(&format!("{id}:change:{index}"), change.clone())),
                        )),
                    )
                })
                .into_any_element()
        }
        Block::FileSearch { pattern, results } => v_flex()
            .w_full()
            .gap(px(8.))
            .children(pattern.as_ref().map(|pattern| mono_frame(cx).text_color(hsla(p.foreground).opacity(0.85)).child(selectable(&format!("{id}:pattern"), pattern.clone()))))
            .when(!results.is_empty(), |this| {
                this.child(v_flex().gap(px(4.)).children(results.iter().enumerate().map(|(index, (location, preview))| {
                    v_flex()
                        .child(mono_xs(cx).text_color(hsla(p.foreground).opacity(0.8)).child(selectable(&format!("{id}:result:{index}"), location.clone())))
                        .children(preview.as_ref().map(|preview| {
                            div().mt(px(2.)).text_color(hsla(p.muted_foreground)).child(selectable(&format!("{id}:preview:{index}"), preview.clone()))
                        }))
                })))
            })
            .into_any_element(),
        Block::WebSearch { patterns, results } => v_flex()
            .w_full()
            .gap(px(8.))
            .children(patterns.as_ref().map(|patterns| mono_frame(cx).text_color(hsla(p.foreground).opacity(0.85)).child(selectable(&format!("{id}:patterns"), patterns.clone()))))
            .when(!results.is_empty(), |this| {
                this.child(v_flex().gap(px(6.)).children(results.iter().enumerate().map(|(index, result)| web_result(&format!("{id}:{index}"), result, cx))))
            })
            .into_any_element(),
    }
}

fn web_result(id: &str, result: &WebResult, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let title = match &result.href {
        Some(href) => {
            let href = href.clone();
            h_flex()
                .id(SharedString::from(format!("{id}:link")))
                .gap(px(4.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.foreground))
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(result.title.clone())
                .child(Icon::ExternalLink.render(px(12.), hsla(p.foreground)))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&href);
                })
                .into_any_element()
        }
        None => div().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(selectable(&format!("{id}:title"), result.title.clone())).into_any_element(),
    };
    v_flex()
        .child(title)
        .children(result.snippet.as_ref().map(|snippet| div().text_color(hsla(p.muted_foreground)).child(selectable(&format!("{id}:snippet"), snippet.clone()))))
        .into_any_element()
}

/// `StructuredValue`: capped at 320, muted, highlighted when it carries runs.
fn structured(id: &str, text: &Text, cx: &App) -> AnyElement {
    Capped { id: SharedString::from(id.to_owned()), text: text.clone(), max_height: 320., color: hsla(palette(cx).muted_foreground) }.into_any_element()
}

fn render_part(id: &str, part: &CallPart, output_slot: &mut Option<Option<AnyElement>>, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    match part {
        CallPart::Command(command) => div().text_color(hsla(p.foreground).opacity(0.85)).child(selectable(id, command.clone())).into_any_element(),
        CallPart::Args(text) => {
            let mut lines = selectable(id, text.text.clone());
            if let Some(highlights) = &text.highlights {
                lines = lines.highlights(highlights.clone());
            }
            div().text_color(hsla(p.foreground).opacity(0.85)).child(lines).into_any_element()
        }
        CallPart::Key(key) => div().text_color(hsla(p.muted_foreground)).child(key.clone()).into_any_element(),
        CallPart::Structured(text) => structured(id, text, cx),
        CallPart::Output { images, line } => {
            if let Some(slot) = output_slot.take() {
                return slot.unwrap_or_else(|| div().into_any_element());
            }
            let images: Vec<AnyElement> =
                images.iter().enumerate().map(|(index, source)| tool_output_image(SharedString::from(format!("{id}:image:{index}")), source.clone(), TOOL_OUTPUT_IMAGE, cx)).collect();
            let line = line.as_ref().map(|line| match line {
                Line::Text(text) => {
                    Capped { id: SharedString::from(format!("{id}:output")), text: Text::plain(text.clone()), max_height: 320., color: hsla(p.muted_foreground) }.into_any_element()
                }
                Line::Note(note) => div().italic().text_color(hsla(p.muted_foreground)).child(selectable(&format!("{id}:note"), note.clone())).into_any_element(),
                Line::Failed(error) => div().text_color(hsla(p.destructive)).child(selectable(&format!("{id}:error"), error.clone())).into_any_element(),
            });
            v_flex().w_full().min_w_0().gap(px(6.)).items_start().children(images).children(line).into_any_element()
        }
        CallPart::Exit(text) => div().text_color(hsla(p.destructive)).child(selectable(id, text.clone())).into_any_element(),
    }
}

/// A read's paths or a skill's arguments: plain mono text capped at 256, in `secondary_label`.
pub fn plain_body(entry_id: &str, text: &str, cx: &App) -> AnyElement {
    let capped = Capped {
        id: SharedString::from(format!("plain:{entry_id}")),
        text: Text::plain(text.to_owned()),
        max_height: 256.,
        color: hsla(palette(cx).secondary_label),
    };
    mono_frame(cx).child(capped).into_any_element()
}

/// `QuestionAnswerHistory`: each question, its answer indented beneath, and the files given.
pub fn question_history(entry_id: &str, questions: &[AnsweredQuestion], cx: &App) -> AnyElement {
    let p = palette(cx);
    let muted = hsla(p.muted_foreground);
    let rows = questions.iter().enumerate().map(|(index, question)| {
        let id = format!("answer:{entry_id}:{index}");
        v_flex()
            .gap(px(4.))
            .children(question.question.as_ref().map(|text| div().text_color(muted).child(selectable(&format!("{id}:q"), text.clone()))))
            .children(question.answer.as_ref().map(|text| div().ml(px(12.)).text_color(muted).child(selectable(&format!("{id}:a"), text.clone()))))
            .when(!question.attachments.is_empty(), |this| {
                this.child(h_flex().flex_wrap().gap(px(8.)).children(question.attachments.iter().map(|name| div().underline().child(name.clone()))))
            })
    });
    v_flex().ml(px(28.)).mt(px(8.)).gap(px(8.)).text_size(px(14.)).line_height(px(20.)).children(rows).into_any_element()
}
