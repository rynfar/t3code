//! The inspector an open tool row shows: `V2ItemInspector.tsx` (`ToolCallBody`, `StructuredValue`,
//! `ToolOutput`) and `FetchedToolOutput`, over the rules in `t3_core::timeline::inspector`. Its text
//! is selectable, long blocks scroll in their own capped scrollers, JSON arguments are highlighted
//! with the captures the web's highlighter uses, and a tool's images come from the media cache.
//! What it shows is derived once per item and fetch, without colors, and kept in a bounded cache
//! across frames; a large item is derived on the background executor. Colors are resolved when
//! drawn, once per theme.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EntityId, FontWeight, Global, HighlightStyle, Hsla, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::{ActiveTheme, Rope, h_flex, v_flex};
use serde_json::Value;
use t3_core::json_text::{JsonToken, is_json_text, js_trim, json_tokens};
use t3_core::media::MediaSource;
use t3_core::timeline::{
    AnsweredQuestion, FetchedDetail, OutputLine, SEARCH_RESULT, TOOL_OUTPUT_IMAGE, ToolCallLines, external_web_link, fetched_turn_item, file_change_line, format_workspace_relative_path,
    is_javascript_repl_tool, questions_text, search_result_location, todo_text, with_visible_control_characters,
};
use t3_core::turn_item::{TurnItem, TurnItemKind};
use t3_core::vcs::format_diff_count;

use super::media::{answer_attachment, tool_output_image};
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

/// Items with less text than this are derived during render; larger ones on the background
/// executor, so a megabyte of output never parses on the UI thread.
const INLINE_BUDGET: usize = 16 * 1024;
/// Texts longer than this draw without syntax colors.
const HIGHLIGHT_BUDGET: usize = 64 * 1024;
/// Derived items kept, most recently drawn first.
const CACHE_LIMIT: usize = 32;
/// Background derives at once; more wait their turn, most recently asked-for first.
const RUNNING_LIMIT: usize = 4;

#[derive(IntoElement)]
pub struct ItemInspector {
    id: SharedString,
    /// The view drawing it, notified when a background derive lands.
    owner: EntityId,
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
    pub fn new(entry_id: &str, item: Arc<TurnItem>, fetch: Fetch, mode: Mode, owner: EntityId) -> Self {
        ItemInspector {
            id: SharedString::from(format!("inspector:{entry_id}")),
            owner,
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

/// A text's runs by what colors them; the colors are looked up when drawn.
enum Runs {
    Plain,
    /// Argument keys, in `muted_foreground`.
    Keys(Vec<Range<usize>>),
    /// JSON, by the capture the web's highlighter gives each run.
    Json(Vec<(Range<usize>, JsonToken)>),
    /// TypeScript, by its parsed tree.
    Code(Box<SyntaxHighlighter>),
}

/// The theme colors are resolved against: the palette and the syntax theme, by identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThemeKey(usize, usize);

impl ThemeKey {
    fn of(cx: &App) -> Self {
        ThemeKey(std::ptr::from_ref(palette(cx)) as usize, Arc::as_ptr(&cx.theme().highlight_theme) as usize)
    }
}

/// A text with its runs, for a selectable block, and its colors under the last theme drawn.
struct Text {
    text: SharedString,
    runs: Runs,
    resolved: RefCell<Option<(ThemeKey, Option<Highlights>)>>,
}

impl Text {
    fn new(text: impl Into<SharedString>, runs: Runs) -> Self {
        Text { text: text.into(), runs, resolved: RefCell::new(None) }
    }

    fn plain(text: impl Into<SharedString>) -> Self {
        Self::new(text, Runs::Plain)
    }

    fn highlights(&self, cx: &App) -> Option<Highlights> {
        self.highlights_with(ThemeKey::of(cx), || resolve(&self.text, &self.runs, cx))
    }

    /// The colors under `theme`, resolved again only when the theme changed.
    fn highlights_with(&self, theme: ThemeKey, resolve: impl FnOnce() -> Option<Highlights>) -> Option<Highlights> {
        if let Some((resolved_for, highlights)) = self.resolved.borrow().as_ref()
            && *resolved_for == theme
        {
            return highlights.clone();
        }
        let highlights = resolve();
        *self.resolved.borrow_mut() = Some((theme, highlights.clone()));
        highlights
    }
}

fn resolve(text: &str, runs: &Runs, cx: &App) -> Option<Highlights> {
    let theme = cx.theme().highlight_theme.clone();
    match runs {
        Runs::Plain => None,
        Runs::Keys(keys) => {
            let muted = HighlightStyle { color: Some(hsla(palette(cx).muted_foreground)), ..HighlightStyle::default() };
            Some(keys.iter().map(|range| (range.clone(), muted)).collect())
        }
        Runs::Json(tokens) => Some(tokens.iter().filter_map(|(range, token)| theme.style(token.capture()).map(|style| (range.clone(), style))).collect()),
        Runs::Code(highlighter) => {
            Some(highlighter.styles(&(0..text.len()), &*theme).into_iter().filter(|(_, style)| *style != HighlightStyle::default()).collect())
        }
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
#[derive(Clone)]
struct Key {
    item: Arc<TurnItem>,
    fetch: Fetch,
    mode: Mode,
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
        fetch && Arc::ptr_eq(&self.item, &other.item) && (self.mode, self.streamed) == (other.mode, other.streamed) && self.workspace_root == other.workspace_root
    }

    fn derive(&self) -> Vec<Block> {
        derive_blocks(&self.item, &self.fetch, self.mode, self.workspace_root.as_deref(), self.streamed)
    }

    /// Whether the item is small enough to derive during render.
    fn inline(&self) -> bool {
        let mut budget = Budget(INLINE_BUDGET);
        budget.item(&self.item)
            && match &self.fetch {
                Fetch::Loaded(item) => budget.item(item),
                _ => true,
            }
    }
}

/// Bytes left before an item is too large to derive during render. Every string the inspector
/// draws spends it, and a walk stops as soon as it runs out.
struct Budget(usize);

impl Budget {
    fn spend(&mut self, bytes: usize) -> bool {
        match self.0.checked_sub(bytes) {
            Some(left) => {
                self.0 = left;
                true
            }
            None => {
                self.0 = 0;
                false
            }
        }
    }

    fn text(&mut self, text: Option<&str>) -> bool {
        self.spend(text.map_or(0, str::len))
    }

    fn value(&mut self, value: &Value) -> bool {
        match value {
            Value::String(text) => self.spend(text.len()),
            Value::Array(items) => items.iter().all(|item| self.spend(1) && self.value(item)),
            Value::Object(object) => object.iter().all(|(key, item)| self.spend(key.len()) && self.value(item)),
            _ => self.spend(8),
        }
    }

    fn values(&mut self, values: Option<&Vec<Value>>) -> bool {
        values.into_iter().flatten().all(|value| self.value(value))
    }

    /// Spends the item's text; `false` once the budget runs out.
    fn item(&mut self, item: &TurnItem) -> bool {
        match &item.kind {
            TurnItemKind::CommandExecution { input, output, .. } => self.spend(input.len()) && self.text(output.as_deref()),
            TurnItemKind::DynamicTool { tool_name, input, output, .. } => {
                self.text(tool_name.as_deref()) && self.value(input) && output.as_ref().is_none_or(|output| self.value(output))
            }
            TurnItemKind::FileChange { file_name, diff_str, changes, .. } => {
                self.spend(file_name.len())
                    && self.text(diff_str.as_deref())
                    && changes.iter().flatten().all(|change| {
                        let parts = [Some(change.operation.as_str()), Some(change.path.as_str()), change.old_path.as_deref(), change.file_type.as_deref(), change.mime_type.as_deref()];
                        parts.into_iter().all(|part| self.text(part))
                    })
            }
            TurnItemKind::FileSearch { pattern, results } => self.text(pattern.as_deref()) && self.values(results.as_ref()),
            TurnItemKind::WebSearch { patterns, results } => {
                patterns.iter().flatten().all(|pattern| self.spend(pattern.len() + 1)) && self.values(results.as_ref())
            }
            TurnItemKind::ApprovalRequest { prompt, .. } => self.text(prompt.as_deref()),
            TurnItemKind::UserInputRequest { questions, .. } => questions.iter().all(|question| self.value(question)),
            TurnItemKind::Notification { detail, .. } => self.text(detail.as_deref()),
            TurnItemKind::SystemNotice { message } => self.spend(message.len()),
            TurnItemKind::Error { failure, .. } => self.spend(failure.message.len()),
            TurnItemKind::TodoList { steps, .. } => steps.iter().all(|step| self.spend(step.text.len() + 2)),
            _ => true,
        }
    }
}

struct Prepared {
    blocks: Vec<Block>,
}

enum Slot {
    /// Waiting for a background derive to free up.
    Queued { waiters: Vec<EntityId> },
    Running { waiters: Vec<EntityId>, _task: Task<()> },
    Ready(Rc<Prepared>),
}

struct Entry {
    key: Key,
    used: u64,
    slot: Slot,
}

/// Derived inspectors, kept while rows scroll in and out: at most `CACHE_LIMIT` entries, of
/// which at most `RUNNING_LIMIT` derive at once.
#[derive(Default)]
pub struct PreparedCache {
    entries: Vec<Entry>,
    tick: u64,
}

struct Store(Entity<PreparedCache>);

impl Global for Store {}

fn cache(cx: &mut App) -> Entity<PreparedCache> {
    if let Some(store) = cx.try_global::<Store>() {
        return store.0.clone();
    }
    let cache = cx.new(|_| PreparedCache::default());
    cx.set_global(Store(cache.clone()));
    cache
}

impl PreparedCache {
    /// The derived blocks for `key`; `None` while a background derive is due, after which
    /// `owner` is notified.
    fn get(&mut self, key: Key, owner: EntityId, cx: &mut Context<Self>) -> Option<Rc<Prepared>> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key.same(&key)) {
            entry.used = tick;
            return match &mut entry.slot {
                Slot::Ready(prepared) => Some(prepared.clone()),
                Slot::Queued { waiters } | Slot::Running { waiters, .. } => {
                    if !waiters.contains(&owner) {
                        waiters.push(owner);
                    }
                    None
                }
            };
        }
        if key.inline() {
            let prepared = Rc::new(Prepared { blocks: key.derive() });
            self.entries.push(Entry { key, used: tick, slot: Slot::Ready(prepared.clone()) });
            let dropped = self.trim();
            wake(dropped, cx);
            return Some(prepared);
        }
        self.entries.push(Entry { key, used: tick, slot: Slot::Queued { waiters: vec![owner] } });
        let dropped = self.trim();
        wake(dropped, cx);
        self.start_queued(cx);
        None
    }

    /// Starts queued derives while fewer than `RUNNING_LIMIT` run.
    fn start_queued(&mut self, cx: &mut Context<Self>) {
        while let Some(index) = self.next_to_start() {
            let entry = &mut self.entries[index];
            let input = entry.key.clone();
            let task = cx.spawn(async move |this, cx| {
                let derived = cx.background_executor().spawn(async move {
                    let blocks = input.derive();
                    (input, blocks)
                });
                let derived = derived.await;
                let _ = this.update(cx, |cache, cx| cache.finish(derived, cx));
            });
            let waiters = match &mut entry.slot {
                Slot::Queued { waiters } => std::mem::take(waiters),
                _ => Vec::new(),
            };
            entry.slot = Slot::Running { waiters, _task: task };
        }
    }

    /// The most recently asked-for queued entry, when a derive is free.
    fn next_to_start(&self) -> Option<usize> {
        let running = self.entries.iter().filter(|entry| matches!(entry.slot, Slot::Running { .. })).count();
        if running >= RUNNING_LIMIT {
            return None;
        }
        self.entries.iter().enumerate().filter(|(_, entry)| matches!(entry.slot, Slot::Queued { .. })).max_by_key(|(_, entry)| entry.used).map(|(index, _)| index)
    }

    /// Lands a background derive, wakes the views waiting for it, and starts the next one.
    fn finish(&mut self, (key, blocks): (Key, Vec<Block>), cx: &mut Context<Self>) {
        let mut waiters = Vec::new();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key.same(&key))
            && let Slot::Running { waiters: waiting, .. } = std::mem::replace(&mut entry.slot, Slot::Ready(Rc::new(Prepared { blocks })))
        {
            waiters = waiting;
        }
        waiters.extend(self.trim());
        self.start_queued(cx);
        wake(waiters, cx);
    }

    /// Drops the least recently asked-for entries past `CACHE_LIMIT`: ready ones first, then
    /// queued ones. Running ones stay, and there are never more than `RUNNING_LIMIT`. Returns the
    /// views that waited on a dropped entry, which must draw again to ask for it again.
    fn trim(&mut self) -> Vec<EntityId> {
        let mut dropped = Vec::new();
        while self.entries.len() > CACHE_LIMIT {
            let oldest = |queued: bool| {
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| if queued { matches!(entry.slot, Slot::Queued { .. }) } else { matches!(entry.slot, Slot::Ready(_)) })
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(index, _)| index)
            };
            let Some(index) = oldest(false).or_else(|| oldest(true)) else { break };
            if let Slot::Queued { waiters } = self.entries.swap_remove(index).slot {
                dropped.extend(waiters);
            }
        }
        dropped
    }
}

/// Redraws `views` after the current update, so a view drawing now is not notified mid-draw.
fn wake(mut views: Vec<EntityId>, cx: &mut Context<PreparedCache>) {
    views.sort();
    views.dedup();
    if views.is_empty() {
        return;
    }
    cx.defer(move |cx| {
        for view in views {
            cx.notify(view);
        }
    });
}

impl RenderOnce for ItemInspector {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let key = Key {
            item: self.item.clone(),
            fetch: self.fetch.clone(),
            mode: self.mode,
            streamed: self.output_slot.is_some(),
            workspace_root: self.workspace_root.clone(),
        };
        let owner = self.owner;
        let prepared = cache(cx).update(cx, |cache, cx| cache.get(key, owner, cx));
        let ItemInspector { id, mut output_slot, mut open_diff, mode, .. } = self;
        let p = palette(cx);
        let children: Vec<AnyElement> = prepared
            .iter()
            .flat_map(|prepared| prepared.blocks.iter())
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

fn derive_blocks(wire: &TurnItem, fetch: &Fetch, mode: Mode, workspace_root: Option<&str>, streamed: bool) -> Vec<Block> {
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
            let mut parts = call_parts(ToolCallLines::command(input), None);
            // The streamed panel stands in for the output; `render_block` puts it here.
            parts.push(if streamed { CallPart::Output { images: Vec::new(), line: None } } else { output_part() });
            parts.extend(exit_code.filter(|code| *code != 0).map(|code| CallPart::Exit(format!("exit {code}").into())));
            vec![Block::Call(parts)]
        }
        TurnItemKind::DynamicTool { tool_name, input, .. } => {
            let mut parts = call_parts(ToolCallLines::args(input), tool_name.as_deref());
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
fn call_parts(call: ToolCallLines, tool_name: Option<&str>) -> Vec<CallPart> {
    let mut parts = Vec::new();
    if let Some(command) = call.command {
        parts.push(CallPart::Command(with_visible_control_characters(&command).into()));
    }
    let mut lines = String::new();
    let mut keys: Vec<Range<usize>> = Vec::new();
    let flush = |lines: &mut String, keys: &mut Vec<Range<usize>>, parts: &mut Vec<CallPart>| {
        if !lines.is_empty() {
            parts.push(CallPart::Args(Text::new(std::mem::take(lines), Runs::Keys(std::mem::take(keys)))));
        }
    };
    for (key, value) in call.args.into_iter().flatten() {
        if key == "code" && is_javascript_repl_tool(tool_name) {
            flush(&mut lines, &mut keys, &mut parts);
            parts.push(CallPart::Key(key.into()));
            parts.push(CallPart::Structured(typescript(value)));
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
        parts.push(CallPart::Structured(json(text)));
    }
    parts
}

/// JSON in the runs the web's highlighter gives it; anything else, or too much of it, plain.
fn json(text: String) -> Text {
    if text.len() > HIGHLIGHT_BUDGET || !is_json_text(&text) {
        return Text::plain(text);
    }
    let tokens = json_tokens(&text);
    Text::new(text, Runs::Json(tokens))
}

fn typescript(code: String) -> Text {
    if code.len() > HIGHLIGHT_BUDGET {
        return Text::plain(code);
    }
    let mut highlighter = SyntaxHighlighter::new("typescript");
    highlighter.update(None, &Rope::from_str(&code), None);
    Text::new(code, Runs::Code(Box::new(highlighter)))
}

/// Selectable text in a scroller capped at `max_height`: drags past its edge scroll it.
#[derive(IntoElement)]
struct Capped {
    id: SharedString,
    text: SharedString,
    highlights: Option<Highlights>,
    max_height: f32,
    color: Hsla,
}

impl RenderOnce for Capped {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window.use_keyed_state(ElementId::Name(SharedString::from(format!("{}:scroll", self.id))), cx, |_, _| ScrollHandle::new()).read(cx).clone();
        let mut text = SelectableText::new(SharedString::from(format!("{}:text", self.id)), self.text).autoscroll(&scroll);
        if let Some(highlights) = self.highlights {
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
    let color = hsla(palette(cx).muted_foreground);
    Capped { id: SharedString::from(id.to_owned()), text: text.text.clone(), highlights: text.highlights(cx), max_height: 320., color }.into_any_element()
}

fn render_part(id: &str, part: &CallPart, output_slot: &mut Option<Option<AnyElement>>, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    match part {
        CallPart::Command(command) => div().text_color(hsla(p.foreground).opacity(0.85)).child(selectable(id, command.clone())).into_any_element(),
        CallPart::Args(text) => {
            let mut lines = selectable(id, text.text.clone());
            if let Some(highlights) = text.highlights(cx) {
                lines = lines.highlights(highlights);
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
                    Capped { id: SharedString::from(format!("{id}:output")), text: text.clone(), highlights: None, max_height: 320., color: hsla(p.muted_foreground) }.into_any_element()
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
        text: SharedString::from(text.to_owned()),
        highlights: None,
        max_height: 256.,
        color: hsla(palette(cx).secondary_label),
    };
    mono_frame(cx).child(capped).into_any_element()
}

/// `QuestionAnswerHistory`: each question, its answer indented beneath, and the files given.
pub fn question_history(entry_id: &str, questions: &[AnsweredQuestion], cx: &mut App) -> AnyElement {
    let muted = hsla(palette(cx).muted_foreground);
    let mut rows = Vec::with_capacity(questions.len());
    for (index, question) in questions.iter().enumerate() {
        let id = format!("answer:{entry_id}:{index}");
        let files: Vec<AnyElement> = question
            .attachments
            .iter()
            .enumerate()
            .map(|(file, attachment)| answer_attachment(SharedString::from(format!("{id}:file:{file}")), attachment, cx))
            .collect();
        rows.push(
            v_flex()
                .gap(px(4.))
                .children(question.question.as_ref().map(|text| div().text_color(muted).child(selectable(&format!("{id}:q"), text.clone()))))
                .children(question.answer.as_ref().map(|text| div().ml(px(12.)).text_color(muted).child(selectable(&format!("{id}:a"), text.clone()))))
                .when(!files.is_empty(), |this| this.child(h_flex().flex_wrap().gap(px(8.)).children(files))),
        );
    }
    v_flex().ml(px(28.)).mt(px(8.)).gap(px(8.)).text_size(px(14.)).line_height(px(20.)).children(rows).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(output: &str) -> Arc<TurnItem> {
        let item = serde_json::json!({
            "id": "item", "threadId": "thread", "runId": null, "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
            "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-10-01T00:00:00Z",
            "type": "dynamic_tool", "toolName": "fetch", "input": { "url": "https://example.com" }, "output": output,
        });
        Arc::new(serde_json::from_value(item).expect("item decodes"))
    }

    fn key(item: Arc<TurnItem>, fetch: Fetch) -> Key {
        Key { item, fetch, mode: Mode::Full, streamed: false, workspace_root: None }
    }

    #[test]
    fn large_items_derive_off_the_ui_thread() {
        assert!(key(tool("short"), Fetch::None).inline());
        assert!(!key(tool(&"x".repeat(INLINE_BUDGET)), Fetch::None).inline());
        let fetched = Fetch::Loaded(tool(&"x".repeat(INLINE_BUDGET)));
        assert!(!key(tool(""), fetched).inline(), "the fetched output counts too");
    }

    fn search(body: serde_json::Value) -> Arc<TurnItem> {
        let mut item = serde_json::json!({
            "id": "search", "threadId": "thread", "runId": null, "nodeId": null, "parentItemId": null, "ordinal": 1, "status": "completed",
            "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-10-01T00:00:00Z",
        });
        item.as_object_mut().expect("object").extend(body.as_object().expect("object").clone());
        Arc::new(serde_json::from_value(item).expect("item decodes"))
    }

    #[test]
    fn every_drawn_string_counts_toward_the_inline_budget() {
        let big = "x".repeat(INLINE_BUDGET + 1);
        assert!(!key(search(serde_json::json!({ "type": "file_search", "pattern": big })), Fetch::None).inline(), "a file search's pattern");
        assert!(!key(search(serde_json::json!({ "type": "web_search", "patterns": [big] })), Fetch::None).inline(), "a web search's queries");
        let halves = ["y".repeat(INLINE_BUDGET / 2), "z".repeat(INLINE_BUDGET / 2)];
        assert!(!key(search(serde_json::json!({ "type": "web_search", "patterns": halves })), Fetch::None).inline(), "queries add up");
        assert!(key(search(serde_json::json!({ "type": "file_search", "pattern": "fn main" })), Fetch::None).inline());
    }

    fn entry(slot: Slot, used: u64) -> Entry {
        Entry { key: key(tool("x"), Fetch::None), used, slot }
    }

    fn running() -> Slot {
        Slot::Running { waiters: Vec::new(), _task: Task::ready(()) }
    }

    fn queued() -> Slot {
        Slot::Queued { waiters: Vec::new() }
    }

    #[test]
    fn the_cache_stays_bounded_while_derives_wait() {
        let mut cache = PreparedCache::default();
        cache.entries.extend((0..RUNNING_LIMIT as u64).map(|used| entry(running(), used)));
        cache.entries.extend((0..CACHE_LIMIT as u64 * 2).map(|used| entry(queued(), 100 + used)));
        let _ = cache.trim();
        assert_eq!(cache.entries.len(), CACHE_LIMIT);
        assert_eq!(cache.entries.iter().filter(|entry| matches!(entry.slot, Slot::Running { .. })).count(), RUNNING_LIMIT, "running derives stay");
        assert!(cache.entries.iter().all(|entry| matches!(entry.slot, Slot::Running { .. }) || entry.used >= 100 + CACHE_LIMIT as u64 + RUNNING_LIMIT as u64), "the oldest queued ones go");

        let mut cache = PreparedCache::default();
        cache.entries.push(entry(queued(), 50));
        cache.entries.extend((0..CACHE_LIMIT as u64).map(|used| entry(Slot::Ready(Rc::new(Prepared { blocks: Vec::new() })), used)));
        let _ = cache.trim();
        assert_eq!(cache.entries.len(), CACHE_LIMIT);
        assert!(cache.entries.iter().any(|entry| matches!(entry.slot, Slot::Queued { .. })), "ready entries go before waiting ones");
    }

    #[test]
    fn a_view_whose_waiting_derive_is_dropped_is_woken_to_ask_again() {
        let (side_chat, main) = (EntityId::from(1u64), EntityId::from(2u64));
        let mut cache = PreparedCache::default();
        cache.entries.extend((0..RUNNING_LIMIT as u64).map(|used| entry(Slot::Running { waiters: vec![main], _task: Task::ready(()) }, used)));
        cache.entries.push(entry(Slot::Queued { waiters: vec![side_chat] }, 10));
        cache.entries.extend((0..CACHE_LIMIT as u64).map(|used| entry(Slot::Queued { waiters: vec![main] }, 20 + used)));
        let woken = cache.trim();
        assert!(woken.contains(&side_chat), "the side chat's dropped row is woken, though only the main view's derives run");
        assert!(!cache.entries.iter().any(|entry| matches!(&entry.slot, Slot::Queued { waiters } if waiters.contains(&side_chat))));
    }

    #[test]
    fn only_a_few_derives_run_at_once_newest_first() {
        let mut cache = PreparedCache::default();
        cache.entries.extend((0..RUNNING_LIMIT as u64).map(|used| entry(running(), used)));
        cache.entries.push(entry(queued(), 10));
        cache.entries.push(entry(queued(), 20));
        assert_eq!(cache.next_to_start(), None, "every derive is busy");
        cache.entries.remove(0);
        assert_eq!(cache.next_to_start().map(|index| cache.entries[index].used), Some(20), "the most recently asked-for one starts next");
    }

    #[test]
    fn text_past_the_budget_draws_plain() {
        assert!(matches!(json("{\"a\": 1}".into()).runs, Runs::Json(_)));
        let big = format!("[{}0]", "0,".repeat(HIGHLIGHT_BUDGET / 2));
        assert!(matches!(json(big).runs, Runs::Plain));
        assert!(matches!(json("not json".into()).runs, Runs::Plain));
        assert!(matches!(typescript("x".repeat(HIGHLIGHT_BUDGET + 1)).runs, Runs::Plain));
    }

    #[test]
    fn colors_resolve_again_when_the_theme_changes() {
        let style = |color: u32| -> Option<Highlights> {
            Some([(0..3, HighlightStyle { color: Some(hsla(color)), ..HighlightStyle::default() })].into_iter().collect())
        };
        let text = Text::new("key value", Runs::Keys(std::iter::once(0..3).collect()));
        let (light, other_light) = (ThemeKey(1, 10), ThemeKey(2, 10));
        assert_eq!(text.highlights_with(light, || style(0x111111ff)), style(0x111111ff));
        assert_eq!(text.highlights_with(light, || style(0x999999ff)), style(0x111111ff), "the same theme keeps its colors");
        assert_eq!(text.highlights_with(other_light, || style(0x222222ff)), style(0x222222ff), "another palette resolves again");
        assert_eq!(text.highlights_with(ThemeKey(2, 11), || style(0x333333ff)), style(0x333333ff), "another syntax theme too");
    }
}
