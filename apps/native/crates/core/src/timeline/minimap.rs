//! The timeline minimap's model. Items and previews port
//! `apps/web/src/components/chat/timelineMinimapItems.ts`; the rail geometry, visible range,
//! current item and gutter rules port the minimap half of `MessagesTimeline.logic.ts`
//! (`resolveTimelineMinimap*`, `isTimelineMinimapRowVisible`).

use std::borrow::Borrow;
use std::sync::Arc;

use markdown::mdast::Node;

use super::{Role, RowKind, TimelineRow};
use crate::composer::attachments::is_attachment_only_prompt;

/// `TIMELINE_MINIMAP_MIN_ITEMS`: fewer user messages show no minimap.
pub const MIN_ITEMS: usize = 2;
/// px between neighbouring strips until the rail reaches its cap.
pub const ITEM_SPACING: f32 = 8.;
/// rem: the rail's cap is the window height less this (`calc(100vh - 18rem)`).
pub const MAX_HEIGHT_INSET: f32 = 288.;
/// rem: the hit strip's width while a preview is open (`22rem`).
pub const EXPANDED_HIT_STRIP_WIDTH: f32 = 352.;
/// px a jumped-to message sits below the top (`scrollToIndex`'s `viewOffset`).
pub const JUMP_VIEW_OFFSET: f32 = 24.;
pub const PREVIOUS_TURN: &str = "Previous turn";
pub const NEXT_TURN: &str = "Next turn";
pub const USER_MESSAGE: &str = "User message";

const PERSISTENT_GUTTER: f32 = 48.;
const HIT_STRIP_LEFT: f32 = 12.;
const HIT_STRIP_MAX_WIDTH: f32 = 40.;
const NAVIGATION_REACH: f32 = 14.;
/// The preview shows three lines; parsing is bounded while long responses stream.
const PREVIEW_PARSE_LIMIT: usize = 1_500;

/// `TimelineMinimapItem`: one per user message, with its turn's final answer.
#[derive(Debug, Clone, PartialEq)]
pub struct MinimapItem {
    pub id: String,
    pub row_index: usize,
    /// Empty for an attachment-only prompt, which previews as "User message".
    pub user_text: Arc<str>,
    pub assistant_text: Option<Arc<str>>,
}

/// `resolveTimelineMinimapPreview`: the hover card's two lines, whitespace collapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinimapPreview {
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
}

/// `deriveTimelineMinimapItems`.
pub fn derive_minimap_items<R: Borrow<TimelineRow>>(rows: &[R]) -> Vec<MinimapItem> {
    let mut items = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let row = row.borrow();
        let RowKind::Message { message, .. } = &row.kind else { continue };
        if message.role != Role::User {
            continue;
        }
        items.push(MinimapItem {
            id: row.id.clone(),
            row_index: index,
            user_text: if is_attachment_only_prompt(&message.text) { Arc::from("") } else { message.text.clone() },
            assistant_text: final_assistant_text(&rows[index + 1..]),
        });
    }
    items
}

/// `resolveFinalAssistantTextForTurn`: the last assistant message before the next user message.
fn final_assistant_text<R: Borrow<TimelineRow>>(rows: &[R]) -> Option<Arc<str>> {
    let mut last = None;
    for row in rows {
        let RowKind::Message { message, .. } = &row.borrow().kind else { continue };
        match message.role {
            Role::User => break,
            Role::Assistant => last = Some(message.text.clone()),
        }
    }
    last
}

pub fn resolve_minimap_preview(item: &MinimapItem) -> MinimapPreview {
    MinimapPreview {
        user_text: compact(&item.user_text),
        assistant_text: item.assistant_text.as_deref().and_then(|text| compact(&markdown_text(utf16_prefix(text, PREVIEW_PARSE_LIMIT)))),
    }
}

/// `compactMinimapPreview`: `\s+` runs become one space, trimmed; empty is `None`.
fn compact(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split(|c: char| c.is_whitespace() || c == '\u{feff}').filter(|word| !word.is_empty()).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// `text.slice(0, limit)`, counted in UTF-16 units as JavaScript does, never splitting a char.
fn utf16_prefix(text: &str, limit: usize) -> &str {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        units += c.len_utf16();
        if units > limit {
            return &text[..index];
        }
    }
    text
}

/// The plain text of GFM markdown, as `markdownNodeText` reads remark's tree. markdown-rs builds
/// the same mdast.
fn markdown_text(source: &str) -> String {
    match markdown::to_mdast(source, &markdown::ParseOptions::gfm()) {
        Ok(tree) => node_text(&tree),
        Err(_) => source.to_owned(),
    }
}

fn node_text(node: &Node) -> String {
    match node {
        Node::Html(html) => html_text(&html.value),
        Node::Break(_) => " ".to_owned(),
        Node::Text(text) => text.value.clone(),
        Node::InlineCode(code) => code.value.clone(),
        Node::Code(code) => code.value.clone(),
        Node::Math(math) => math.value.clone(),
        Node::InlineMath(math) => math.value.clone(),
        Node::Yaml(yaml) => yaml.value.clone(),
        Node::Toml(toml) => toml.value.clone(),
        Node::Image(image) => image.alt.clone(),
        Node::ImageReference(image) => image.alt.clone(),
        _ => {
            let Some(children) = node.children() else { return String::new() };
            let inline = matches!(
                node,
                Node::Paragraph(_) | Node::Heading(_) | Node::TableCell(_) | Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_) | Node::Link(_) | Node::LinkReference(_)
            );
            let text = children.iter().map(node_text).collect::<Vec<_>>().join(if inline { "" } else { " " });
            // A thread autolink's only text is its URL.
            if matches!(node, Node::Link(_)) && is_thread_link_href(&text) { "a thread".to_owned() } else { text }
        }
    }
}

/// `parseThreadLinkHref(href) !== null` (`packages/shared/src/threadLinks.ts`).
fn is_thread_link_href(href: &str) -> bool {
    let id = |value: &str| !value.trim().is_empty();
    if let Some(thread) = href.strip_prefix("t3-thread://v1/") {
        return id(thread);
    }
    href.strip_prefix("t3-thread://v2/").and_then(|path| path.split_once('/')).is_some_and(|(environment, thread)| id(environment) && id(thread))
}

const BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "blockquote", "br", "dd", "details", "div", "dl", "dt", "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5",
    "h6", "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "summary", "table", "tbody", "td", "th", "thead", "tr", "ul",
];

/// Raw HTML's text: comments dropped, block-level tags read as a space and other tags as nothing.
fn html_text(value: &str) -> String {
    let mut uncommented = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("<!--") {
        uncommented.push_str(&rest[..start]);
        rest = rest[start + 4..].find("-->").map_or("", |end| &rest[start + 4 + end + 3..]);
    }
    uncommented.push_str(rest);

    let mut out = String::with_capacity(uncommented.len());
    let mut rest = uncommented.as_str();
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        match html_tag(&rest[start..]) {
            Some((name, len)) => {
                if BLOCK_TAGS.iter().any(|tag| tag.eq_ignore_ascii_case(name)) {
                    out.push(' ');
                }
                rest = &rest[start + len..];
            }
            None => {
                out.push('<');
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A tag at the start of `text` (`<\/?([a-z][\w-]*)(?:[^"'<>]|"[^"]*"|'[^']*')*>`, ignoring
/// case): its name and byte length.
fn html_tag(text: &str) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut index = 1;
    if bytes.get(index) == Some(&b'/') {
        index += 1;
    }
    let name_start = index;
    if !bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    while bytes.get(index).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-') {
        index += 1;
    }
    let name = &text[name_start..index];
    loop {
        match *bytes.get(index)? {
            b'>' => return Some((name, index + 1)),
            b'<' => return None,
            quote @ (b'"' | b'\'') => index += 1 + text[index + 1..].find(quote as char)? + 1,
            _ => index += 1,
        }
    }
}

/// `resolveTimelineMinimapHeightStyle`: 8px per gap, capped at `max_height`.
pub fn rail_height(item_count: usize, max_height: f32) -> f32 {
    let natural = (item_count.saturating_sub(1) as f32 * ITEM_SPACING).max(1.);
    natural.min(max_height).max(0.)
}

/// `resolveTimelineMinimapTopPercent`.
pub fn top_percent(index: usize, item_count: usize) -> f32 {
    if item_count <= 1 {
        return 0.;
    }
    index.min(item_count - 1) as f32 / (item_count - 1) as f32 * 100.
}

/// `resolveTimelineMinimapIndexFromPointer`.
pub fn index_from_pointer(item_count: usize, rail_top: f32, rail_height: f32, pointer_y: f32) -> Option<usize> {
    if item_count == 0 || rail_height <= 0. {
        return None;
    }
    let progress = ((pointer_y - rail_top) / rail_height).clamp(0., 1.);
    Some(((progress * (item_count - 1) as f32).round() as usize).min(item_count - 1))
}

/// `resolveTimelineMinimapVisibleRange`: the first and last of `row_count` rows that show between
/// `scroll` and `scroll + scroll_length`, a row with no height counting as 1px tall. Row `i`
/// starts at `position_at(i)` and ends where row `i + 1` starts. The web rechecks LegendList's
/// rendered buffer; GPUI's height tree places every row, so this binary-searches all of them.
pub fn visible_range(row_count: usize, scroll: f32, scroll_length: f32, position_at: impl Fn(usize) -> f32) -> Option<(usize, usize)> {
    let bottom = scroll + scroll_length;
    let ends_above_scroll = |index: usize| {
        let top = position_at(index);
        top + (position_at(index + 1) - top).max(1.) <= scroll
    };
    let start = partition_point(row_count, ends_above_scroll);
    let end = partition_point(row_count, |index| position_at(index) < bottom);
    (start < end).then(|| (start, end - 1))
}

/// The first index in `0..len` where `before` turns false; `before` holds for a prefix.
fn partition_point(len: usize, before: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let mid = low + (high - low) / 2;
        if before(mid) { low = mid + 1 } else { high = mid }
    }
    low
}

/// `isTimelineMinimapRowVisible`.
pub fn is_row_visible(row_index: usize, range: Option<(usize, usize)>) -> bool {
    range.is_some_and(|(start, end)| (start..=end).contains(&row_index))
}

/// `resolveTimelineMinimapCurrentIndex`: the first visible user message, otherwise the turn being
/// read (the last one above the view).
pub fn current_index(range: Option<(usize, usize)>, row_indices: impl IntoIterator<Item = usize>) -> Option<usize> {
    let (start, end) = range?;
    let mut preceding = None;
    for (index, row_index) in row_indices.into_iter().enumerate() {
        if row_index >= start {
            return if row_index <= end { Some(index) } else { preceding };
        }
        preceding = Some(index);
    }
    preceding
}

fn side_gutter(viewport_width: f32, content_width: f32) -> f32 {
    if !viewport_width.is_finite() || viewport_width <= 0. || !content_width.is_finite() {
        return 0.;
    }
    ((viewport_width - viewport_width.min(content_width)) / 2.).max(0.)
}

/// `resolveTimelineMinimapHasPersistentGutter`: with room beside the lane the minimap always
/// shows; otherwise only while hovered.
pub fn has_persistent_gutter(viewport_width: f32, content_width: f32) -> bool {
    side_gutter(viewport_width, content_width) >= PERSISTENT_GUTTER
}

/// `resolveTimelineMinimapHitStripWidth`: 0 leaves the rail inert.
pub fn hit_strip_width(viewport_width: f32, content_width: f32) -> f32 {
    (side_gutter(viewport_width, content_width).floor() - HIT_STRIP_LEFT).clamp(0., HIT_STRIP_MAX_WIDTH)
}

/// `resolveTimelineMinimapNavigationInteractive`.
pub fn navigation_interactive(hit_strip_width: f32) -> bool {
    hit_strip_width >= NAVIGATION_REACH
}

/// Where to scroll so list item `item` sits `view_offset` below the top: the item to anchor,
/// the offset into it, and the distance still to scroll up once the items above are measured.
/// Unmeasured items have no height in GPUI's tree, so the walk up stops at the first one.
pub fn jump_anchor(item: usize, view_offset: f32, size_at: impl Fn(usize) -> f32) -> (usize, f32, f32) {
    let (mut index, mut remaining) = (item, view_offset);
    while remaining > 0. && index > 0 {
        let size = size_at(index - 1);
        if size <= 0. {
            break;
        }
        index -= 1;
        if size >= remaining {
            return (index, size - remaining, 0.);
        }
        remaining -= size;
    }
    (index, 0., remaining)
}

/// A left-button event on the rail or anywhere else in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailPointerEvent {
    Down { on_rail: bool },
    Up { on_rail: bool },
    Move { on_rail: bool, pressed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailPointerAction {
    Ignore,
    /// Follow the pointer with the active item.
    Track,
    /// Jump to the item under the pointer.
    Jump,
}

/// The rail's press state (`pressedOnStripRef` and `passThrough`): a press that starts on the
/// rail scrubs along it and jumps where it is released; a drag from elsewhere passes over it.
/// Every release and button-free move, on the rail or not, ends the press, as the web's window
/// `mouseup` and `mousemove` listeners do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RailPointer {
    pressed_on_rail: bool,
}

impl RailPointer {
    pub fn handle(&mut self, event: RailPointerEvent) -> RailPointerAction {
        match event {
            RailPointerEvent::Down { on_rail } => {
                self.pressed_on_rail = on_rail;
                RailPointerAction::Ignore
            }
            RailPointerEvent::Up { on_rail } => {
                let jump = on_rail && self.pressed_on_rail;
                self.pressed_on_rail = false;
                if jump { RailPointerAction::Jump } else { RailPointerAction::Ignore }
            }
            RailPointerEvent::Move { on_rail, pressed } => {
                if !pressed {
                    self.pressed_on_rail = false;
                }
                if on_rail && (!pressed || self.pressed_on_rail) { RailPointerAction::Track } else { RailPointerAction::Ignore }
            }
        }
    }
}

#[cfg(test)]
mod tests;
