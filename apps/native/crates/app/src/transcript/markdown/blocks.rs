//! Custom GPUI `TextView` block extensions (`ALERT_NODE`, `DETAILS_NODE`, `MATH_NODE`,
//! `MERMAID_NODE`, `TABLE_NODE`) and fenced code block metadata lookup (`title`, closed state).

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EntityId, FontWeight, Global, InteractiveElement, IntoElement, ParentElement, RenderImage, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, markdown_ast as mdast};
use gpui_component::{ActiveTheme, h_flex, v_flex};
pub use t3_core::markdown::code::CodeFenceMetaMap;
use t3_core::markdown::alerts_details::{
    AlertKind, CustomOrderedList, DetailsBlock, GithubAlert, parse_custom_ordered_list,
    parse_details_fence, parse_github_alert,
};
use t3_core::markdown::code::{is_code_fence_closed, is_math_language, is_mermaid_language};
use t3_core::markdown::math::render_tex_unicode;
use t3_core::markdown::mermaid::render_mermaid_svg;
use t3_core::markdown::table::{ColumnAlignment, TableData};

use super::links::MarkdownActionContext;
use crate::icons::Icon;
use crate::selectable_text::{SelectableText, SelectableTextHandle};
use crate::theme::{
    SlotState, SlotStyleOverride, ThemeSlot, apply_slot_override, hsla, palette, slot_style,
    theme_slot_state_background_layer,
};
use crate::transcript::{media, parts};

pub const ALERT_NODE: &str = "t3-alert";
pub const DETAILS_NODE: &str = "t3-details-block";
pub const ORDERED_LIST_NODE: &str = "t3-ordered-list";
pub const MATH_NODE: &str = "t3-math-block";
pub const MERMAID_NODE: &str = "t3-mermaid-block";
pub const TABLE_NODE: &str = "t3-table-block";
pub const CODE_BLOCK_NODE: &str = "t3-code-block";

fn is_horizontal_wheel(event: &ScrollWheelEvent) -> bool {
    let (dx, dy) = match event.delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (l.x, l.y),
    };
    dx != 0.0 && dx.abs() > dy.abs()
}

/// Prevents horizontal trackpad scrolling inside code blocks, tables, math, and mermaid code
/// scrollers from bubbling up to `Workspace`'s `HistorySwipeNavigation` listener (`AP-SWIPE`),
/// while letting vertical wheel events bubble normally to the transcript list.
fn stop_horizontal_wheel(event: &ScrollWheelEvent, _window: &mut Window, cx: &mut App) {
    if is_horizontal_wheel(event) {
        cx.stop_propagation();
    }
}

#[derive(Clone)]
pub enum NestedSelection {
    Markdown(Entity<TextViewState>),
    Alert {
        entity: Entity<TextViewState>,
        plain_body: String,
        clipboard_markdown: String,
    },
    Selectable {
        handle: SelectableTextHandle,
        rendered: String,
        raw_fallback: Option<String>,
    },
}

pub type NestedViews = Arc<Mutex<HashMap<String, NestedSelection>>>;
pub type SharedActionContext = Arc<Mutex<Option<MarkdownActionContext>>>;
pub type SharedCodeMeta = Arc<Mutex<CodeFenceMetaMap>>;
pub type SharedMathEnabled = Arc<AtomicBool>;

#[derive(Clone)]
struct AlertNodeData {
    key: String,
    alert: GithubAlert,
    plain_body: String,
    clipboard_markdown: String,
}

#[derive(Clone)]
struct OrderedListNodeData {
    key: String,
    list: CustomOrderedList,
}

#[derive(Clone)]
struct DetailsNodeData {
    key: String,
    details: DetailsBlock,
}

#[derive(Clone)]
struct MathNodeData {
    key: String,
    tex: String,
    display: String,
}

#[derive(Clone)]
struct MermaidNodeData {
    key: String,
    code: Arc<str>,
}

#[derive(Clone)]
struct TableNodeData {
    key: String,
    data: TableData,
    raw_markdown: String,
    cell_markdown: Vec<Vec<String>>,
}

#[derive(Clone)]
struct CodeBlockNodeData {
    key: String,
    raw_markdown: String,
}

#[derive(Clone)]
pub(super) struct RichMarkdownMedia {
    pub(super) parse_document: media::MediaDocument,
    pub(super) gallery_document: media::MediaDocument,
    pub(super) scope: media::MediaScopeSource,
    pub(super) selections: media::MediaSelections,
}

struct ChildTextState {
    last_body: String,
    entity: Entity<TextViewState>,
    extensions: MarkdownExtensions,
}

struct DetailsUiState {
    open: bool,
    last_body: String,
    entity: Entity<TextViewState>,
    extensions: MarkdownExtensions,
}

struct MathUiState {
    show_tex: bool,
}

#[derive(Clone, Copy)]
struct MermaidExtent {
    width: f32,
    aspect: f32,
}

impl MermaidExtent {
    fn layout<E: Styled>(self, element: E) -> E {
        element.w(px(self.width)).max_w_full().aspect_ratio(self.aspect)
    }
}

#[derive(Clone)]
struct MermaidPreview {
    svg: Arc<str>,
    image: Arc<RenderImage>,
    extent: MermaidExtent,
    pixel_width: u32,
    pixel_height: u32,
}

impl MermaidPreview {
    const GUTTER: u32 = 2;

    fn render(source: &str, dark: bool, renderer: gpui::SvgRenderer) -> Result<Self, String> {
        let rendered = render_mermaid_svg(source, dark)?;
        let scale = (2048. / rendered.width.max(rendered.height)).min(2.);
        let frame = renderer.render_single_frame(rendered.svg.as_bytes(), scale)
            .map_err(|error| format!("SVG render failed: {error}"))?;
        let size = frame.size(0);
        let (width, height) = (size.width.0 as u32, size.height.0 as u32);
        let pixels = frame.as_bytes(0).filter(|_| width > 0 && height > 0)
            .ok_or_else(|| "SVG render returned no pixels".to_owned())?;
        let gutter = Self::GUTTER;
        let padded = image::RgbaImage::from_fn(width + 2 * gutter, height + 2 * gutter, |x, y| {
            let x = x.saturating_sub(gutter).min(width - 1);
            let y = y.saturating_sub(gutter).min(height - 1);
            let offset = (y as usize * width as usize + x as usize) * 4;
            image::Rgba(pixels[offset..offset + 4].try_into().unwrap())
        });
        Ok(Self {
            svg: Arc::from(rendered.svg),
            image: Arc::new(RenderImage::new(vec![image::Frame::new(padded)])),
            extent: MermaidExtent {
                width: rendered.width.max(240.),
                aspect: rendered.width / rendered.height,
            },
            pixel_width: width,
            pixel_height: height,
        })
    }

    fn surface(self) -> impl IntoElement {
        self.extent.layout(gpui::canvas(
            |_, _, _| (),
            move |visible, (), window, _| {
                if !window.content_mask().bounds.intersects(&visible) {
                    return;
                }
                let pad_x = visible.size.width * (Self::GUTTER as f32 / self.pixel_width as f32);
                let pad_y = visible.size.height * (Self::GUTTER as f32 / self.pixel_height as f32);
                let fitted = gpui::Bounds::new(
                    gpui::point(visible.origin.x - pad_x, visible.origin.y - pad_y),
                    gpui::size(visible.size.width + 2. * pad_x, visible.size.height + 2. * pad_y),
                );
                if window.paint_image_fitted(visible, fitted, gpui::Corners::default(), self.image.clone(), 0, false).is_ok() {
                    // GPUI carries presentation callbacks forward with cached paint.
                    window.on_present(move || { let _ = &self.image; });
                }
            },
        ))
    }
}

type MermaidKey = (Arc<str>, bool);

enum MermaidSlot {
    Pending(Vec<EntityId>, gpui::Task<()>),
    Ready(MermaidPreview),
    Failed,
}

#[derive(Default)]
struct MermaidStore {
    entries: VecDeque<(MermaidKey, MermaidSlot, u64)>,
    frame: u64,
    drawing: bool,
    // Keep geometry across theme changes and raster eviction.
    extents: VecDeque<(Arc<str>, MermaidExtent)>,
    bytes: usize,
}

struct MermaidCache(Entity<MermaidStore>);
impl Global for MermaidCache {}

impl MermaidStore {
    const ENTRY_LIMIT: usize = 64;
    const EXTENT_LIMIT: usize = 256;
    const BYTE_LIMIT: usize = 32 * 1024 * 1024;

    fn get(
        &mut self,
        key: MermaidKey,
        owner: EntityId,
        cx: &mut Context<Self>,
    ) -> (Option<MermaidPreview>, Option<MermaidExtent>, bool) {
        let (source, dark) = &key;
        if source.len() > 64 * 1024 {
            return (None, None, true);
        }
        if !self.drawing {
            self.drawing = true;
            self.frame = self.frame.wrapping_add(1);
            let cache = cx.entity().downgrade();
            // Wait until every block in this draw has had a chance to touch its slot.
            cx.defer(move |cx| {
                let _ = cache.update(cx, |store, cx| {
                    store.drawing = false;
                    store.trim(None, cx);
                });
            });
        }
        let extent = self.extents.iter().position(|(code, _)| code == source)
            .and_then(|index| self.extents.remove(index))
            .map(|entry| {
                let extent = entry.1;
                self.extents.push_back(entry);
                extent
            });
        let mut slot = self.entries.iter().position(|(candidate, _, _)| candidate == &key)
            .and_then(|index| self.entries.remove(index))
            .map_or_else(|| {
                let renderer = cx.svg_renderer();
                let source = source.clone();
                let dark = *dark;
                let completed = key.clone();
                let task = cx.spawn(async move |cache, cx| {
                    let preview = cx.background_executor().spawn(async move {
                        MermaidPreview::render(&source, dark, renderer)
                    }).await;
                    let _ = cache.update(cx, |cache, cx| cache.finish(completed, preview, cx));
                });
                MermaidSlot::Pending(Vec::new(), task)
            }, |(_, slot, _)| slot);
        let result = match &mut slot {
            MermaidSlot::Ready(preview) => (Some(preview.clone()), Some(preview.extent), false),
            MermaidSlot::Failed => (None, None, true),
            MermaidSlot::Pending(waiters, _) => {
                if !waiters.contains(&owner) {
                    waiters.push(owner);
                }
                (None, extent, false)
            }
        };
        self.entries.push_back((key, slot, self.frame));
        result
    }

    fn finish(
        &mut self,
        key: MermaidKey,
        preview: Result<MermaidPreview, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.entries.iter().position(|(candidate, slot, _)| {
            candidate == &key && matches!(slot, MermaidSlot::Pending(..))
        }) else {
            return;
        };
        let Some((_, MermaidSlot::Pending(waiters, task), touched)) = self.entries.remove(index) else {
            return;
        };
        task.detach();
        let slot = match preview {
            Ok(preview) => {
                self.bytes += Self::preview_bytes(&preview);
                self.remember_extent(key.0.clone(), preview.extent);
                MermaidSlot::Ready(preview)
            }
            Err(_) => MermaidSlot::Failed,
        };
        self.entries.push_back((key.clone(), slot, touched));
        self.trim(Some(&key), cx);
        let app: &mut App = cx;
        for waiter in waiters {
            app.notify(waiter);
        }
    }

    fn remember_extent(&mut self, code: Arc<str>, extent: MermaidExtent) {
        self.extents.retain(|(candidate, _)| candidate != &code);
        self.extents.push_back((code, extent));
        if self.extents.len() > Self::EXTENT_LIMIT {
            self.extents.pop_front();
        }
    }

    fn preview_bytes(preview: &MermaidPreview) -> usize {
        let padding = 2 * MermaidPreview::GUTTER;
        (preview.pixel_width + padding) as usize * (preview.pixel_height + padding) as usize * 4
            + preview.svg.len()
    }

    fn trim(&mut self, completed: Option<&MermaidKey>, cx: &mut Context<Self>) {
        while self.entries.len() > Self::ENTRY_LIMIT || self.bytes > Self::BYTE_LIMIT {
            let over_entry_limit = self.entries.len() > Self::ENTRY_LIMIT;
            let index = self.entries.iter().enumerate()
                .filter(|(_, (key, slot, touched))| {
                    completed != Some(key)
                        && *touched != self.frame
                        && match slot {
                            MermaidSlot::Pending(..) => over_entry_limit,
                            MermaidSlot::Ready(preview) => Arc::strong_count(&preview.image) == 1,
                            MermaidSlot::Failed => true,
                        }
                })
                .min_by_key(|(_, (_, slot, _))| matches!(slot, MermaidSlot::Pending(..)))
                .map(|(index, _)| index);
            let Some(index) = index else { break };
            if let Some((_, MermaidSlot::Ready(preview), _)) = self.entries.remove(index) {
                self.bytes -= Self::preview_bytes(&preview);
                cx.defer(move |cx| cx.drop_image(preview.image, None));
            }
        }
    }
}

fn mermaid_cache(cx: &mut App) -> Entity<MermaidStore> {
    if let Some(cache) = cx.try_global::<MermaidCache>() {
        return cache.0.clone();
    }
    let cache = cx.new(|_| MermaidStore::default());
    cx.set_global(MermaidCache(cache.clone()));
    cache
}

pub(super) fn hash_source_key(key: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn salt_source_hash(base: u64, key: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    base.hash(&mut hasher);
    key.hash(&mut hasher);
    hasher.finish()
}

fn update_child_text(entity: &Entity<TextViewState>, previous: &mut String, next: &str, cx: &mut App) -> bool {
    if previous.as_str() == next {
        return false;
    }
    if !previous.is_empty()
        && let Some(delta) = next.strip_prefix(previous.as_str())
        && !delta.is_empty()
    {
        previous.push_str(delta);
        entity.update(cx, |tv, cx| tv.push_str(delta, cx));
        return true;
    }
    *previous = next.to_owned();
    entity.update(cx, |tv, cx| tv.set_text(next, cx));
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_rich_extensions_with_media_options(
    base: MarkdownExtensions,
    source_hash: u64,
    action_ctx: SharedActionContext,
    nested_views: NestedViews,
    code_meta: SharedCodeMeta,
    math_enabled: SharedMathEnabled,
    media: RichMarkdownMedia,
    keep_ordered_list_numbers: bool,
) -> MarkdownExtensions {
    attach_extensions_inner(
        base,
        source_hash,
        action_ctx,
        nested_views,
        code_meta,
        math_enabled,
        Some(media),
        true,
        true,
        keep_ordered_list_numbers,
    )
}

fn child_media_base(media: Option<&RichMarkdownMedia>, body: &str) -> MarkdownExtensions {
    let Some(media_ctx) = media else { return MarkdownExtensions::default() };
    let parse_document = media::media_document_with_inherited(&media_ctx.parse_document, Arc::from(body));
    media::markdown_extensions_with_gallery(
        parse_document,
        media_ctx.gallery_document.clone(),
        media_ctx.scope.clone(),
        media_ctx.selections.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
fn attach_extensions_inner(
    base: MarkdownExtensions,
    source_hash: u64,
    action_ctx: SharedActionContext,
    nested_views: NestedViews,
    code_meta: SharedCodeMeta,
    math_enabled: SharedMathEnabled,
    media: Option<RichMarkdownMedia>,
    intercept_tables: bool,
    intercept_code_blocks: bool,
    keep_ordered_list_numbers: bool,
) -> MarkdownExtensions {
    let ctx_for_child = action_ctx.clone();
    let nested_for_child = nested_views.clone();
    let meta_for_child = code_meta.clone();
    let math_for_parser = math_enabled.clone();
    let math_for_child = math_enabled;

    let with_parsers = base.block_parser(move |node, context| {
        parse_rich_block(
            source_hash,
            node,
            context,
            intercept_tables,
            intercept_code_blocks,
            keep_ordered_list_numbers,
            math_for_parser.load(Ordering::Relaxed),
        )
    });

    let ctx_alert = ctx_for_child.clone();
    let nested_alert = nested_for_child.clone();
    let meta_alert = meta_for_child.clone();
    let math_alert = math_for_child.clone();
    let media_alert = media.clone();

    let ctx_details = ctx_for_child.clone();
    let nested_details = nested_for_child.clone();
    let meta_details = meta_for_child.clone();
    let math_details = math_for_child.clone();
    let media_details = media.clone();

    let ctx_list = ctx_for_child.clone();
    let nested_list = nested_for_child.clone();
    let meta_list = meta_for_child.clone();
    let math_list = math_for_child.clone();
    let media_list = media.clone();

    let nested_math = nested_for_child.clone();

    let ctx_mermaid = ctx_for_child.clone();
    let nested_mermaid = nested_for_child.clone();

    let ctx_table = ctx_for_child.clone();
    let nested_table = nested_for_child.clone();
    let meta_table = meta_for_child.clone();
    let math_table = math_for_child.clone();
    let media_table = media.clone();

    let ctx_code = ctx_for_child;
    let nested_code = nested_for_child;
    let meta_code = meta_for_child;
    let math_code = math_for_child;
    let media_code = media;

    with_parsers
        .block_renderer(ALERT_NODE, move |node, window, cx| {
            let Some(data) = node.data::<AlertNodeData>() else { return div().into_any_element() };
            let env = RichBlockEnv { source_hash, action_ctx: &ctx_alert, nested_views: &nested_alert, code_meta: &meta_alert, math_enabled: &math_alert, media: media_alert.as_ref() };
            render_alert_block(data, env, window, cx)
        })
        .block_renderer(DETAILS_NODE, move |node, window, cx| {
            let Some(data) = node.data::<DetailsNodeData>() else { return div().into_any_element() };
            let env = RichBlockEnv { source_hash, action_ctx: &ctx_details, nested_views: &nested_details, code_meta: &meta_details, math_enabled: &math_details, media: media_details.as_ref() };
            render_details_block(data, env, window, cx)
        })
        .block_renderer(ORDERED_LIST_NODE, move |node, window, cx| {
            let Some(data) = node.data::<OrderedListNodeData>() else { return div().into_any_element() };
            let env = RichBlockEnv { source_hash, action_ctx: &ctx_list, nested_views: &nested_list, code_meta: &meta_list, math_enabled: &math_list, media: media_list.as_ref() };
            render_ordered_list_block(data, env, window, cx)
        })
        .block_renderer(MATH_NODE, move |node, window, cx| {
            let Some(data) = node.data::<MathNodeData>() else { return div().into_any_element() };
            render_math_block(data, &nested_math, window, cx)
        })
        .block_renderer(MERMAID_NODE, move |node, window, cx| {
            let Some(data) = node.data::<MermaidNodeData>() else { return div().into_any_element() };
            render_mermaid_block(data, &ctx_mermaid, &nested_mermaid, window, cx)
        })
        .block_renderer(TABLE_NODE, move |node, window, cx| {
            let Some(data) = node.data::<TableNodeData>() else { return div().into_any_element() };
            let env = RichBlockEnv { source_hash, action_ctx: &ctx_table, nested_views: &nested_table, code_meta: &meta_table, math_enabled: &math_table, media: media_table.as_ref() };
            render_table_block(data, env, window, cx)
        })
        .block_renderer(CODE_BLOCK_NODE, move |node, window, cx| {
            let Some(data) = node.data::<CodeBlockNodeData>() else { return div().into_any_element() };
            let env = RichBlockEnv { source_hash, action_ctx: &ctx_code, nested_views: &nested_code, code_meta: &meta_code, math_enabled: &math_code, media: media_code.as_ref() };
            render_code_block(data, env, window, cx)
        })
}

fn alert_plain_body(bq: &mdast::Blockquote) -> String {
    let raw = mdast::Node::Blockquote(bq.clone()).to_string();
    let trimmed = raw.trim();
    if let Some(after_close) = trimmed.strip_prefix("[!")
        && let Some(end) = after_close.find(']')
    {
        return after_close[end + 1..].trim().to_owned();
    }
    trimmed.to_owned()
}

fn fallback_code_fence(code: &mdast::Code) -> String {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for ch in code.value.chars() {
        if ch == '`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(max_run.max(2) + 1);
    let mut info = String::new();
    if let Some(lang) = code.lang.as_deref() {
        info.push_str(lang);
        if let Some(meta) = code.meta.as_deref()
            && !meta.is_empty()
        {
            info.push(' ');
            info.push_str(meta);
        }
    }
    format!("{fence}{info}\n{}\n{fence}", code.value)
}

fn parse_rich_block(
    source_hash: u64,
    node: &mdast::Node,
    context: &MarkdownParseContext<'_>,
    intercept_tables: bool,
    intercept_code_blocks: bool,
    keep_ordered_list_numbers: bool,
    math_enabled: bool,
) -> Option<MarkdownNode> {
    let key = node_key(source_hash, context.offset(), node);
    match node {
        mdast::Node::Blockquote(bq) => {
            let alert = parse_github_alert(bq, context.source())?;
            let plain_body = alert_plain_body(bq);
            let clipboard_markdown = alert.clipboard_markdown();
            let raw = context.node_source(node).unwrap_or(&alert.body_markdown).to_owned();
            let plain = if alert.body_markdown.is_empty() {
                alert.title.to_owned()
            } else {
                format!("{}\n{}", alert.title, alert.body_markdown)
            };
            Some(
                MarkdownNode::new(
                    ALERT_NODE,
                    AlertNodeData {
                        key,
                        alert,
                        plain_body,
                        clipboard_markdown,
                    },
                )
                .text(plain)
                .markdown(raw),
            )
        }
        mdast::Node::List(list) if keep_ordered_list_numbers && list.ordered => {
            let parsed = parse_custom_ordered_list(list, context.source())?;
            let raw = context
                .node_source(node)
                .map(str::to_owned)
                .unwrap_or_else(|| parsed.raw_markdown.clone());
            let plain = parsed
                .items
                .iter()
                .map(|item| format!("{}. {}", item.number, item.body_markdown))
                .collect::<Vec<_>>()
                .join("\n");
            Some(
                MarkdownNode::new(ORDERED_LIST_NODE, OrderedListNodeData { key, list: parsed })
                    .text(plain)
                    .markdown(raw),
            )
        }
        mdast::Node::Table(tbl) if intercept_tables => {
            let data = TableData::from_mdast(tbl);
            let cell_markdown = TableData::cell_markdown_rows(tbl, context.source());
            let raw = context
                .node_source(node)
                .map(str::to_owned)
                .unwrap_or_else(|| data.to_markdown());
            let plain = data
                .rows
                .iter()
                .map(|r| r.join("\t"))
                .collect::<Vec<_>>()
                .join("\n");
            Some(
                MarkdownNode::new(TABLE_NODE, TableNodeData { key, data, raw_markdown: raw.clone(), cell_markdown })
                    .text(plain)
                    .markdown(raw),
            )
        }
        mdast::Node::Code(code) => {
            let lang = code.lang.as_deref();
            if lang == Some("t3-details")
                && let Some(details) = parse_details_fence(lang, code.meta.as_deref(), &code.value)
            {
                let plain = format!("{}\n{}", details.summary, details.body_markdown);
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(DETAILS_NODE, DetailsNodeData { key, details })
                        .text(plain)
                        .markdown(raw),
                );
            }
            let closed = node
                .position()
                .is_some_and(|p| is_code_fence_closed(context.source(), p.start.offset));
            if math_enabled && (lang == Some("t3-math") || (closed && is_math_language(lang))) {
                let display = render_tex_unicode(&code.value);
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(MATH_NODE, MathNodeData { key, tex: code.value.clone(), display: display.clone() })
                        .text(display)
                        .markdown(raw),
                );
            }
            if closed && is_mermaid_language(lang) {
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(MERMAID_NODE, MermaidNodeData { key, code: Arc::from(code.value.as_str()) })
                        .text(code.value.clone())
                        .markdown(raw),
                );
            }
            if intercept_code_blocks {
                let raw = context
                    .node_source(node)
                    .map(str::to_owned)
                    .unwrap_or_else(|| fallback_code_fence(code));
                return Some(
                    MarkdownNode::new(CODE_BLOCK_NODE, CodeBlockNodeData { key, raw_markdown: raw.clone() })
                        .text(code.value.clone())
                        .markdown(raw),
                );
            }
            None
        }
        _ => None,
    }
}

fn node_key(source_hash: u64, context_offset: usize, node: &mdast::Node) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source_hash.hash(&mut hasher);
    match node {
        mdast::Node::Blockquote(b) => {
            "bq".hash(&mut hasher);
            b.position.as_ref().map(|p| context_offset.saturating_add(p.start.offset)).hash(&mut hasher);
        }
        mdast::Node::Table(t) => {
            "tbl".hash(&mut hasher);
            t.position.as_ref().map(|p| context_offset.saturating_add(p.start.offset)).hash(&mut hasher);
        }
        mdast::Node::Code(c) => {
            "code".hash(&mut hasher);
            c.position.as_ref().map(|p| context_offset.saturating_add(p.start.offset)).hash(&mut hasher);
            c.lang.hash(&mut hasher);
            c.meta.hash(&mut hasher);
        }
        mdast::Node::List(l) => {
            "list".hash(&mut hasher);
            l.position.as_ref().map(|p| context_offset.saturating_add(p.start.offset)).hash(&mut hasher);
        }
        _ => {}
    }
    format!("md-block-{:x}", hasher.finish())
}

#[derive(Clone, Copy)]
struct RichBlockEnv<'a> {
    source_hash: u64,
    action_ctx: &'a SharedActionContext,
    nested_views: &'a NestedViews,
    code_meta: &'a SharedCodeMeta,
    math_enabled: &'a SharedMathEnabled,
    media: Option<&'a RichMarkdownMedia>,
}

fn render_alert_block(
    data: &AlertNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let p = palette(cx);
    let (accent, icon) = match data.alert.kind {
        AlertKind::Insight => (p.info, Icon::Star),
        AlertKind::Note => (p.info, Icon::Info),
        AlertKind::Tip => (p.success, Icon::Sparkles),
        AlertKind::Important => (p.primary, Icon::CircleAlert),
        AlertKind::Warning => (p.warning, Icon::TriangleAlert),
        AlertKind::Caution => (p.destructive, Icon::CircleX),
    };

    let body = data.alert.body_markdown.clone();
    let child_hash = salt_source_hash(source_hash, &data.key);
    let build_ext = |body: &str| {
        attach_extensions_inner(
            child_media_base(media, body),
            child_hash,
            action_ctx.clone(),
            nested_views.clone(),
            code_meta.clone(),
            math_enabled.clone(),
            media.cloned(),
            true,
            true,
            false,
        )
    };
    let key = SharedString::from(format!("alert-body:{}", data.key));
    let state = window.use_keyed_state(key.clone(), cx, |_, cx| ChildTextState {
        last_body: body.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&body, cx).selectable(true)),
        extensions: build_ext(&body),
    });
    let (entity, child_ext) = state.update(cx, |s, cx| {
        let changed = update_child_text(&s.entity, &mut s.last_body, &body, cx);
        if changed && media.is_some() {
            s.extensions = build_ext(&body);
        }
        (s.entity.clone(), s.extensions.clone())
    });
    nested_views.lock().unwrap().insert(
        data.key.clone(),
        NestedSelection::Alert {
            entity: entity.clone(),
            plain_body: data.plain_body.clone(),
            clipboard_markdown: data.clipboard_markdown.clone(),
        },
    );

    let view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );

    let copy_markdown = data.clipboard_markdown.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(accent).opacity(0.32))
        .border_l_4()
        .bg(hsla(accent).opacity(0.08))
        .px(px(12.))
        .py(px(10.))
        .gap(px(6.))
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .child(
                    h_flex()
                        .gap(px(6.))
                        .items_center()
                        .child(icon.render(px(14.), hsla(accent)))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hsla(accent))
                                .child(data.alert.title),
                        ),
                )
                .child(parts::ghost_icon_button(
                    SharedString::from(format!("alert-copy:{}", data.key)),
                    Icon::Copy,
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_markdown.clone()));
                    },
                    cx,
                )),
        )
        .when(!data.alert.body_markdown.is_empty(), |this| this.child(view))
        .into_any_element()
}

fn render_ordered_list_block(
    data: &OrderedListNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let p = palette(cx);
    let rows: Vec<AnyElement> = data
        .list
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let item_key = format!("ol:{}:{}", data.key, index);
            let body = item.body_markdown.clone();
            let item_hash = salt_source_hash(source_hash, &item_key);
            let build_ext = |body: &str| {
                attach_extensions_inner(
                    child_media_base(media, body),
                    item_hash,
                    action_ctx.clone(),
                    nested_views.clone(),
                    code_meta.clone(),
                    math_enabled.clone(),
                    media.cloned(),
                    true,
                    true,
                    true,
                )
            };
            let state = window.use_keyed_state(SharedString::from(item_key.clone()), cx, |_, cx| ChildTextState {
                last_body: body.clone(),
                entity: cx.new(|cx| TextViewState::markdown(&body, cx).selectable(true)),
                extensions: build_ext(&body),
            });
            let (entity, child_ext) = state.update(cx, |s, cx| {
                let changed = update_child_text(&s.entity, &mut s.last_body, &body, cx);
                if changed && media.is_some() {
                    s.extensions = build_ext(&body);
                }
                (s.entity.clone(), s.extensions.clone())
            });
            nested_views.lock().unwrap().insert(item_key, NestedSelection::Markdown(entity.clone()));
            let view = super::style_markdown_internal(
                TextView::new(&entity).markdown_extensions(child_ext),
                action_ctx.lock().unwrap().clone(),
                code_meta.clone(),
                cx,
            );
            h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap(px(8.))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(22.))
                        .text_right()
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("{}.", item.number)),
                )
                .child(div().flex_1().min_w_0().child(view))
                .into_any_element()
        })
        .collect();
    v_flex().w_full().min_w_0().gap(px(4.)).children(rows).into_any_element()
}

fn render_details_block(
    data: &DetailsNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let p = palette(cx);
    let body = data.details.body_markdown.clone();
    let default_open = data.details.open;
    let child_hash = salt_source_hash(source_hash, &data.key);
    let build_ext = |body: &str| {
        attach_extensions_inner(
            child_media_base(media, body),
            child_hash,
            action_ctx.clone(),
            nested_views.clone(),
            code_meta.clone(),
            math_enabled.clone(),
            media.cloned(),
            true,
            true,
            false,
        )
    };
    let key = SharedString::from(format!("details:{}", data.key));
    let state = window.use_keyed_state(key.clone(), cx, |_, cx| DetailsUiState {
        open: default_open,
        last_body: body.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&body, cx).selectable(true)),
        extensions: build_ext(&body),
    });
    let (open, entity, child_ext) = state.update(cx, |s, cx| {
        let changed = update_child_text(&s.entity, &mut s.last_body, &body, cx);
        if changed && media.is_some() {
            s.extensions = build_ext(&body);
        }
        (s.open, s.entity.clone(), s.extensions.clone())
    });
    if open {
        nested_views.lock().unwrap().insert(data.key.clone(), NestedSelection::Markdown(entity.clone()));
    } else {
        nested_views.lock().unwrap().remove(&data.key);
    }

    let body_view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );

    let rtl = data.details.rtl;
    let toggle_state = state.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.card).opacity(0.45))
        .overflow_hidden()
        .child(
            h_flex()
                .id(SharedString::from(format!("details-toggle:{}", data.key)))
                .w_full()
                .when(rtl, |this| this.flex_row_reverse())
                .gap(px(8.))
                .items_center()
                .px(px(12.))
                .py(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(hsla(p.accent).opacity(0.08)))
                .on_click(move |_, _, cx| {
                    toggle_state.update(cx, |s, cx| {
                        s.open = !s.open;
                        cx.notify();
                    });
                })
                .child(
                    if open {
                        Icon::ChevronDown
                    } else if rtl {
                        Icon::ChevronLeft
                    } else {
                        Icon::ChevronRight
                    }
                    .render(px(14.), hsla(p.muted_foreground)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(rtl, |this| this.text_right())
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(data.details.summary.clone()),
                ),
        )
        .when(open && !data.details.body_markdown.is_empty(), |this| {
            this.child(
                div()
                    .w_full()
                    .when(rtl, |this| this.text_right())
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .px(px(12.))
                    .py(px(10.))
                    .child(body_view),
            )
        })
        .into_any_element()
}

fn render_math_block(data: &MathNodeData, nested_views: &NestedViews, window: &mut Window, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let key = SharedString::from(format!("math:{}", data.key));
    let ui = window.use_keyed_state(key, cx, |_, _| MathUiState { show_tex: false });
    let show_tex = ui.read(cx).show_tex;

    let toggle_ui = ui.clone();
    let tex_for_copy = data.tex.clone();
    let shown_text = if show_tex { data.tex.clone() } else { data.display.clone() };
    let group_id = SharedString::from(format!("math-grp:{}", data.key));

    v_flex()
        .w_full()
        .min_w_0()
        .group(group_id.clone())
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.code_block_border))
        .bg(hsla(p.code_block_background))
        .px(px(14.))
        .py(px(10.))
        .gap(px(6.))
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(hsla(p.muted_foreground))
                        .child(if show_tex { "TeX" } else { "Math" }),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .items_center()
                        .child(parts::ghost_text_button(
                            SharedString::from(format!("math-toggle:{}", data.key)),
                            if show_tex { "Rendered" } else { "TeX" },
                            None,
                            move |_, _, cx| {
                                toggle_ui.update(cx, |s, cx| {
                                    s.show_tex = !s.show_tex;
                                    cx.notify();
                                });
                            },
                            cx,
                        ))
                        .child(parts::CopyButton::new(
                            SharedString::from(format!("math-copy:{}", data.key)),
                            "Copy",
                            move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(tex_for_copy.clone())),
                        )),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .on_scroll_wheel(stop_horizontal_wheel)
                .child(
                    div()
                        .id(SharedString::from(format!("math-scroll:{}", data.key)))
                        .w_full()
                        .overflow_x_scroll()
                        .py(px(4.))
                        .child(
                            div()
                                .when(!show_tex, |d| d.flex().justify_center().text_size(px(16.)).line_height(px(24.)))
                                .when(show_tex, |d| {
                                    d.font_family(cx.theme().mono_font_family.clone())
                                        .text_size(cx.theme().mono_font_size)
                                })
                                .text_color(hsla(p.foreground))
                                .child({
                                    let nested = nested_views.clone();
                                    let key = format!("math:{}", data.key);
                                    let rendered = shown_text.clone();
                                    let raw_fallback = (shown_text != data.tex).then(|| data.tex.clone());
                                    SelectableText::new(SharedString::from(format!("math-sel:{}", data.key)), shown_text)
                                        .on_state(move |handle, _| {
                                            nested.lock().unwrap().insert(
                                                key.clone(),
                                                NestedSelection::Selectable {
                                                    handle: handle.clone(),
                                                    rendered: rendered.clone(),
                                                    raw_fallback: raw_fallback.clone(),
                                                },
                                            );
                                        })
                                }),
                        ),
                ),
        )
        .into_any_element()
}

fn render_mermaid_block(
    data: &MermaidNodeData,
    action_ctx: &SharedActionContext,
    nested_views: &NestedViews,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let is_dark = cx.theme().mode.is_dark();
    let key = SharedString::from(format!("mermaid:{}", data.key));
    let state = window.use_keyed_state(key, cx, |_, _| false);
    let show_code = *state.read(cx);
    let owner = window.current_view();
    let (preview, extent, failed) = mermaid_cache(cx).update(cx, |cache, cx| {
        cache.get((data.code.clone(), is_dark), owner, cx)
    });
    let loading = preview.is_none() && !failed;
    let show_diagram = !show_code && preview.is_some();
    let toggle_state = state.clone();
    let copy_code = t3_core::markdown::strip_code_fence_trailing_newline(&data.code);
    let expand_ctx = action_ctx.lock().unwrap().clone();
    let expand_svg = preview.as_ref().map(|preview| preview.svg.clone());

    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.code_block_border))
        .bg(hsla(p.code_block_background))
        .overflow_hidden()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .px(px(12.))
                .py(px(6.))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.6))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .items_center()
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(11.))
                                .text_color(hsla(p.foreground).opacity(0.72))
                                .child("mermaid"),
                        )
                        .when(failed, |this| {
                            this.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(hsla(p.muted_foreground))
                                    .child("Preview unavailable — showing code"),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .items_center()
                        .when(preview.is_some(), |this| {
                            this.child(parts::ghost_text_button(
                                SharedString::from(format!("mermaid-toggle:{}", data.key)),
                                if show_code { "Diagram" } else { "Code" },
                                Some(Icon::CodeXml),
                                move |_, _, cx| {
                                    toggle_state.update(cx, |show, cx| {
                                        *show = !*show;
                                        cx.notify();
                                    });
                                },
                                cx,
                            ))
                        })
                        .when(show_diagram && expand_ctx.as_ref().and_then(|c| c.thread_view.as_ref()).is_some(), |this| {
                            let ctx = expand_ctx.clone().unwrap();
                            let svg_for_expand = expand_svg.clone();
                            this.child(parts::ghost_icon_button(
                                SharedString::from(format!("mermaid-expand:{}", data.key)),
                                Icon::Maximize2,
                                move |_, window, cx| {
                                    if let Some(tv) = ctx.thread_view.as_ref().and_then(|w| w.upgrade())
                                        && let Some(svg) = svg_for_expand.as_ref() {
                                        let data_url = svg_data_url(svg);
                                        tv.update(cx, |view, cx| {
                                            view.open_media(
                                                vec![crate::media::ViewerItem::image(
                                                    t3_core::media::MediaSource::Direct(data_url),
                                                    "Mermaid diagram",
                                                )],
                                                0,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                },
                                cx,
                            ))
                        })
                        .child(parts::CopyButton::new(
                            SharedString::from(format!("mermaid-copy:{}", data.key)),
                            "Copy",
                            move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_code.clone())),
                        )),
                ),
        )
        .child(if show_diagram {
            nested_views.lock().unwrap().remove(&format!("mermaid-code:{}", data.key));
            div()
                .w_full()
                .p(px(12.))
                .flex()
                .justify_center()
                .children(preview.map(MermaidPreview::surface))
                .into_any_element()
        } else if loading && !show_code {
            let fallback = match extent {
                Some(extent) => extent.layout(div()).into_any_element(),
                None => div()
                    .h(cx.theme().mono_font_size * 1.375 * data.code.lines().count().max(1) as f32)
                    .into_any_element(),
            };
            div().w_full().p(px(12.)).flex().justify_center().child(fallback).into_any_element()
        } else {
            let nested = nested_views.clone();
            let key = format!("mermaid-code:{}", data.key);
            let code_text = data.code.to_string();
            div()
                .w_full()
                .min_w_0()
                .on_scroll_wheel(stop_horizontal_wheel)
                .child(
                    div()
                        .id(SharedString::from(format!("mermaid-code:{}", data.key)))
                        .w_full()
                        .overflow_x_scroll()
                        .px(px(14.))
                        .py(px(12.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size)
                        .line_height(cx.theme().mono_font_size * 1.375)
                        .text_color(hsla(p.foreground))
                        .child(
                            SelectableText::new(SharedString::from(format!("mermaid-sel:{}", data.key)), code_text.clone())
                                .on_state(move |handle, _| {
                                    nested.lock().unwrap().insert(
                                        key.clone(),
                                        NestedSelection::Selectable {
                                            handle: handle.clone(),
                                            rendered: code_text.clone(),
                                            raw_fallback: None,
                                        },
                                    );
                                }),
                        ),
                )
                .into_any_element()
        })
        .into_any_element()
}

fn render_table_block(
    data: &TableNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let p = palette(cx);
    let has_media = data.cell_markdown.iter().flatten().any(|cell| super::has_image_syntax(cell));
    let table_view = if has_media {
        render_media_table_grid(data, env, window, cx)
    } else {
        let raw = data.raw_markdown.clone();
        let child_hash = salt_source_hash(source_hash, &data.key);
        let key = SharedString::from(format!("table:{}", data.key));
        let state = window.use_keyed_state(key, cx, |_, cx| ChildTextState {
            last_body: raw.clone(),
            entity: cx.new(|cx| TextViewState::markdown(&raw, cx).selectable(true)),
            extensions: attach_extensions_inner(
                MarkdownExtensions::default(),
                child_hash,
                action_ctx.clone(),
                nested_views.clone(),
                code_meta.clone(),
                math_enabled.clone(),
                media.cloned(),
                false,
                false,
                false,
            ),
        });
        let (entity, child_ext) = state.update(cx, |s, cx| {
            let _ = update_child_text(&s.entity, &mut s.last_body, &raw, cx);
            (s.entity.clone(), s.extensions.clone())
        });
        nested_views.lock().unwrap().insert(data.key.clone(), NestedSelection::Markdown(entity.clone()));

        super::style_markdown_internal(
            TextView::new(&entity).markdown_extensions(child_ext),
            action_ctx.lock().unwrap().clone(),
            code_meta.clone(),
            cx,
        )
        .into_any_element()
    };

    let md_copy = data.data.to_markdown();
    let csv_copy = data.data.to_csv();
    let html_copy = data.data.to_html();
    const TABLE_GROUP: &str = "markdown-table";

    v_flex()
        .id(SharedString::from(format!("table:{}", data.key)))
        .group(TABLE_GROUP)
        .w_full()
        .min_w_0()
        .gap(px(4.))
        .child(
            h_flex()
                .justify_end()
                .gap(px(4.))
                .invisible()
                .group_hover(TABLE_GROUP, |style| style.visible())
                .child(parts::CopyButton::new(
                    SharedString::from(format!("table-copy-md:{}", data.key)),
                    "Copy Markdown",
                    move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(md_copy.clone()));
                    },
                ))
                .child(parts::CopyButton::new(
                    SharedString::from(format!("table-copy-csv:{}", data.key)),
                    "Copy CSV",
                    move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(csv_copy.clone()));
                    },
                ))
                .child(parts::CopyButton::new(
                    SharedString::from(format!("table-copy-html:{}", data.key)),
                    "Copy HTML",
                    move |_, cx| {
                        crate::composer::clipboard::write_raw_html(html_copy.clone(), html_copy.clone(), cx);
                    },
                )),
        )
        .child(
            div()
                .id(SharedString::from(format!("table-scroll-guard:{}", data.key)))
                .w_full()
                .min_w_0()
                .border_color(hsla(p.border))
                .on_scroll_wheel(stop_horizontal_wheel)
                .child(table_view),
        )
        .into_any_element()
}

fn render_code_block(
    data: &CodeBlockNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let raw = data.raw_markdown.clone();
    let child_hash = salt_source_hash(source_hash, &data.key);
    let key = SharedString::from(format!("code-block:{}", data.key));
    let state = window.use_keyed_state(key, cx, |_, cx| ChildTextState {
        last_body: raw.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&raw, cx).selectable(true)),
        extensions: attach_extensions_inner(
            MarkdownExtensions::default(),
            child_hash,
            action_ctx.clone(),
            nested_views.clone(),
            code_meta.clone(),
            math_enabled.clone(),
            media.cloned(),
            false,
            false,
            false,
        ),
    });
    let (entity, child_ext) = state.update(cx, |s, cx| {
        let _ = update_child_text(&s.entity, &mut s.last_body, &raw, cx);
        (s.entity.clone(), s.extensions.clone())
    });
    nested_views
        .lock()
        .unwrap()
        .insert(format!("code:{}", data.key), NestedSelection::Markdown(entity.clone()));

    let code_view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );
    let mut card = div()
        .id(SharedString::from(format!("code-block-scroll-guard:{}", data.key)))
        .relative()
        .w_full()
        .min_w_0()
        .on_scroll_wheel(stop_horizontal_wheel);
    let slot_set = slot_style(ThemeSlot::ChatMarkdownCodeblock, cx);
    if let Some(bg_layer) = theme_slot_state_background_layer(
        ThemeSlot::ChatMarkdownCodeblock,
        SlotState::Base,
        px(8.),
        cx,
    ) {
        let mut card_style = gpui::StyleRefinement::default().rounded(px(8.));
        if let Some(set) = &slot_set {
            let card_override = SlotStyleOverride {
                border_gradient: set.base.border_gradient.clone(),
                background: set.base.background.clone(),
                background_color: set.base.background_color,
                backdrop_blur: set.base.backdrop_blur,
                border_color: set.base.border_color,
                border_widths: set.base.border_widths,
                border_radii: set.base.border_radii,
                box_shadows: set.base.box_shadows.clone(),
                ..SlotStyleOverride::default()
            };
            apply_slot_override(&mut card_style, &card_override, cx);
        }
        card.style().corner_radii = card_style.corner_radii;
        card.style().border_widths = card_style.border_widths;
        card.style().border_color = card_style.border_color;
        card.style().box_shadow = card_style.box_shadow;
        card = card.overflow_hidden().child(bg_layer);
    }
    if let Some(set) = slot_set {
        if set.base.display_none == Some(true) {
            card.style().display = Some(gpui::Display::None);
        }
        if !set.hover.is_empty() {
            card = card.hover(|mut style| {
                apply_slot_override(&mut style, &set.hover, cx);
                style
            });
        }
        if !set.active.is_empty() {
            card = card.active(|mut style| {
                apply_slot_override(&mut style, &set.active, cx);
                style
            });
        }
    }
    card.children(crate::theme::inspector_slot_probe(ThemeSlot::ChatMarkdownCodeblock, cx))
        .child(code_view)
        .into_any_element()
}

fn render_media_table_grid(
    data: &TableNodeData,
    env: RichBlockEnv<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let RichBlockEnv { source_hash, action_ctx, nested_views, code_meta, math_enabled, media } = env;
    let p = palette(cx);
    let cols = data.cell_markdown.iter().map(Vec::len).max().unwrap_or(0).max(data.data.align.len()).max(1);
    let row_count = data.cell_markdown.len();
    let mut grid = v_flex().w_full().min_w_0();
    for (row_idx, row_cells) in data.cell_markdown.iter().enumerate() {
        let is_header = row_idx == 0;
        let mut row_el = h_flex()
            .w_full()
            .min_w_0()
            .items_stretch()
            .when(is_header, |row| row.bg(hsla(p.secondary)))
            .when(!is_header && row_idx % 2 == 0, |row| row.bg(hsla(p.muted).opacity(0.25)))
            .when(row_idx + 1 < row_count, |row| row.border_b_1().border_color(hsla(p.border).opacity(0.6)));
        for col_idx in 0..cols {
            let cell_md = row_cells.get(col_idx).cloned().unwrap_or_default();
            let cell_key = format!("{}:{row_idx}:{col_idx}", data.key);
            let cell_hash = salt_source_hash(source_hash, &cell_key);
            let build_ext = |cell_md: &str| {
                attach_extensions_inner(
                    child_media_base(media, cell_md),
                    cell_hash,
                    action_ctx.clone(),
                    nested_views.clone(),
                    code_meta.clone(),
                    math_enabled.clone(),
                    media.cloned(),
                    false,
                    true,
                    false,
                )
            };
            let key = SharedString::from(format!("table-cell:{cell_key}"));
            let state = window.use_keyed_state(key, cx, |_, cx| ChildTextState {
                last_body: cell_md.clone(),
                entity: cx.new(|cx| TextViewState::markdown(&cell_md, cx).selectable(true)),
                extensions: build_ext(&cell_md),
            });
            let (entity, child_ext) = state.update(cx, |s, cx| {
                let changed = update_child_text(&s.entity, &mut s.last_body, &cell_md, cx);
                if changed && media.is_some() {
                    s.extensions = build_ext(&cell_md);
                }
                (s.entity.clone(), s.extensions.clone())
            });
            nested_views.lock().unwrap().insert(
                cell_key,
                NestedSelection::Markdown(entity.clone()),
            );
            let cell_view = super::style_markdown_internal(
                TextView::new(&entity).markdown_extensions(child_ext),
                action_ctx.lock().unwrap().clone(),
                code_meta.clone(),
                cx,
            );
            let align = data.data.align.get(col_idx).copied().unwrap_or(ColumnAlignment::None);
            let cell_el = v_flex()
                .flex_1()
                .min_w(px(120.))
                .px(px(10.))
                .py(px(6.))
                .justify_center()
                .when(is_header, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                .when(align == ColumnAlignment::Center, |cell| cell.items_center().text_center())
                .when(align == ColumnAlignment::Right, |cell| cell.items_end().text_right())
                .when(align == ColumnAlignment::Left || align == ColumnAlignment::None, |cell| cell.items_start().text_left())
                .when(col_idx + 1 < cols, |cell| cell.border_r_1().border_color(hsla(p.border).opacity(0.6)))
                .child(div().max_w_full().min_w_0().child(cell_view));
            row_el = row_el.child(cell_el);
        }
        grid = grid.child(row_el);
    }
    div()
        .id(SharedString::from(format!("table-media-scroll:{}", data.key)))
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .rounded(px(6.))
        .border_1()
        .border_color(hsla(p.border))
        .child(grid)
        .into_any_element()
}

fn svg_data_url(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml,");
    for &b in svg.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

#[cfg(test)]
mod wheel_propagation_tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Context, Render, TouchPhase, point, size};

    use super::*;
    use crate::transcript::markdown::{MarkdownState, MarkdownStore, markdown_view};

    #[gpui::test]
    fn code_blocks_and_tables_stop_horizontal_wheel_but_allow_vertical_scroll(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });

        struct Harness {
            state: MarkdownState,
            bubbled_horizontal: Rc<Cell<usize>>,
            bubbled_vertical: Rc<Cell<usize>>,
        }

        impl Render for Harness {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let h = self.bubbled_horizontal.clone();
                let v = self.bubbled_vertical.clone();
                div()
                    .id("swipe-parent")
                    .size_full()
                    .on_scroll_wheel(move |event, _, _| {
                        if is_horizontal_wheel(event) {
                            h.set(h.get() + 1);
                        } else {
                            v.set(v.get() + 1);
                        }
                    })
                    .child(markdown_view(&self.state, cx))
            }
        }

        let source: Arc<str> = concat!(
            "```rust\n",
            "let very_long_identifier_for_horizontal_scrolling = \"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\";\n",
            "```\n\n",
            "| ColumnOneHeader | ColumnTwoHeader | ColumnThreeHeader | ColumnFourHeader |\n",
            "| --- | --- | --- | --- |\n",
            "| wide_cell_value_alpha | wide_cell_value_beta | wide_cell_value_gamma | wide_cell_value_delta |\n"
        )
        .into();

        let bubbled_horizontal = Rc::new(Cell::new(0usize));
        let bubbled_vertical = Rc::new(Cell::new(0usize));
        let h_ref = bubbled_horizontal.clone();
        let v_ref = bubbled_vertical.clone();

        let (_view, vcx) = cx.add_window_view(|_, cx| {
            let mut store = MarkdownStore::new(cx.entity_id());
            let state = store.state("assistant:wheel-test", &source, cx);
            Harness {
                state,
                bubbled_horizontal: h_ref,
                bubbled_vertical: v_ref,
            }
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.resize(size(px(420.), px(400.)));
            window.draw(cx).clear();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let wheel_event = |pos: gpui::Point<gpui::Pixels>, dx: f32, dy: f32| ScrollWheelEvent {
            position: pos,
            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers: gpui::Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        };

        // Hit inside the code block (near top of viewport).
        let code_pos = point(px(120.), px(40.));
        vcx.simulate_event(wheel_event(code_pos, 32.0, 0.0));
        assert_eq!(bubbled_horizontal.get(), 0, "horizontal wheel inside code block must not bubble to swipe handler");
        vcx.simulate_event(wheel_event(code_pos, 0.0, -24.0));
        assert_eq!(bubbled_vertical.get(), 1, "vertical wheel inside code block must still bubble");

        // Hit inside the table (below the code block).
        let table_pos = point(px(120.), px(145.));
        vcx.simulate_event(wheel_event(table_pos, -36.0, 1.0));
        assert_eq!(bubbled_horizontal.get(), 0, "horizontal wheel inside table must not bubble to swipe handler");
        vcx.simulate_event(wheel_event(table_pos, 0.0, -24.0));
        assert_eq!(bubbled_vertical.get(), 2, "vertical wheel inside table must still bubble");
    }

    #[gpui::test]
    fn code_node_key_distinguishes_identical_blocks_and_is_stable_while_streaming(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });

        struct ProbeHarness {
            entity: Entity<TextViewState>,
            ext: MarkdownExtensions,
        }
        impl Render for ProbeHarness {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                div()
                    .w(px(500.))
                    .child(TextView::new(&self.entity).markdown_extensions(self.ext.clone()))
            }
        }

        let nested_views: NestedViews = Arc::new(Mutex::new(HashMap::new()));
        let action_ctx: SharedActionContext = Arc::new(Mutex::new(None));
        let code_meta: SharedCodeMeta = Arc::new(Mutex::new(CodeFenceMetaMap::default()));
        let math_enabled: SharedMathEnabled = Arc::new(AtomicBool::new(false));
        let ext = attach_extensions_inner(
            MarkdownExtensions::default(),
            hash_source_key("assistant:key-stability"),
            action_ctx,
            nested_views.clone(),
            code_meta,
            math_enabled,
            None,
            true,
            true,
            false,
        );

        let two_identical = "```sh\necho hi\n```\n\n```sh\necho hi";
        let (view, vcx) = cx.add_window_view(|_, cx| {
            let entity = cx.new(|cx| TextViewState::markdown(two_identical, cx).selectable(true));
            ProbeHarness { entity, ext: ext.clone() }
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let mut initial_keys: Vec<String> = nested_views
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("code:"))
            .cloned()
            .collect();
        initial_keys.sort();
        assert_eq!(
            initial_keys.len(),
            2,
            "two identical code blocks at different offsets must produce distinct keyed states: {initial_keys:?}"
        );

        // Append a streamed line to the second block (keeping both blocks' start offsets fixed).
        nested_views.lock().unwrap().clear();
        vcx.update(|window, cx| {
            view.update(cx, |h, cx| {
                h.entity.update(cx, |tv, cx| tv.push_str("\necho more", cx));
            });
            window.draw(cx).clear();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let mut streamed_keys: Vec<String> = nested_views
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("code:"))
            .cloned()
            .collect();
        streamed_keys.sort();
        assert_eq!(
            streamed_keys, initial_keys,
            "streaming content into a code block via push_str must keep its keyed state stable"
        );
    }

    #[gpui::test]
    fn different_source_keys_isolate_blocks_and_rerenders_preserve_child_selection(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
        });

        let nested_a: NestedViews = Arc::new(Mutex::new(HashMap::new()));
        let nested_b: NestedViews = Arc::new(Mutex::new(HashMap::new()));
        let make_ext = |key: &str, nested: NestedViews| {
            attach_extensions_inner(
                MarkdownExtensions::default(),
                hash_source_key(key),
                Arc::new(Mutex::new(None)),
                nested,
                Arc::new(Mutex::new(CodeFenceMetaMap::default())),
                Arc::new(AtomicBool::new(false)),
                None,
                true,
                true,
                false,
            )
        };
        let ext_a = make_ext("assistant:msg-a", nested_a.clone());
        let ext_b = make_ext("assistant:msg-b", nested_b.clone());

        struct DualHarness {
            a: Entity<TextViewState>,
            ext_a: MarkdownExtensions,
            b: Entity<TextViewState>,
            ext_b: MarkdownExtensions,
            tick: usize,
        }
        impl Render for DualHarness {
            fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                let _ = self.tick;
                gpui_component::v_flex()
                    .w(px(500.))
                    .child(TextView::new(&self.a).markdown_extensions(self.ext_a.clone()))
                    .child(TextView::new(&self.b).markdown_extensions(self.ext_b.clone()))
            }
        }

        let (view, vcx) = cx.add_window_view(|_, cx| DualHarness {
            a: cx.new(|cx| TextViewState::markdown("```rs\nlet a = 1;\n```\n", cx).selectable(true)),
            ext_a: ext_a.clone(),
            b: cx.new(|cx| TextViewState::markdown("```rs\nlet a = 1;\n```\n", cx).selectable(true)),
            ext_b: ext_b.clone(),
            tick: 0,
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let keys_a: Vec<String> = nested_a.lock().unwrap().keys().cloned().collect();
        let keys_b: Vec<String> = nested_b.lock().unwrap().keys().cloned().collect();
        assert_eq!(keys_a.len(), 1);
        assert_eq!(keys_b.len(), 1);
        assert_ne!(keys_a, keys_b, "identical offset-0 code blocks across messages must use distinct keys");

        let child_a = match nested_a.lock().unwrap().get(&keys_a[0]).cloned() {
            Some(NestedSelection::Markdown(entity)) => entity,
            _ => panic!("expected child markdown entity"),
        };
        vcx.update(|window, cx| {
            child_a.update(cx, |tv, cx| tv.select_all(cx));
            assert!(!child_a.read(cx).selected_text().is_empty());
            view.update(cx, |h, cx| {
                h.tick += 1;
                cx.notify();
            });
            window.refresh();
            window.draw(cx).clear();
        });
        let selected_after = child_a.read_with(vcx, |tv, _| tv.selected_text());
        assert!(
            selected_after.contains("let a = 1;"),
            "re-rendering enclosing view must preserve cached MarkdownExtensions revision and child selection: got {selected_after:?}"
        );
    }
}

#[cfg(test)]
mod mermaid_tests {
    use super::*;

    #[gpui::test]
    fn three_visible_large_diagrams_each_keep_their_first_raster(cx: &mut gpui::TestAppContext) {
        let keys = ["A", "B", "C"].map(|label| (Arc::from(format!("graph TD\n {label} --> Z")), false));
        let (store, renderer) = cx.update(|cx| (mermaid_cache(cx), cx.svg_renderer()));
        store.update(cx, |store, cx| {
            store.entries.extend(keys.iter().cloned().map(|key| {
                (key, MermaidSlot::Pending(Vec::new(), gpui::Task::ready(())), 0)
            }));
            for key in &keys {
                let (preview, _, failed) = store.get(key.clone(), cx.entity_id(), cx);
                assert!(preview.is_none() && !failed);
            }
        });
        let images = store.update(cx, |store, cx| keys.each_ref().map(|key| {
            let mut preview = MermaidPreview::render(&key.0, key.1, renderer.clone()).unwrap();
            preview.pixel_width = 2048;
            preview.pixel_height = 2048;
            let image = Arc::downgrade(&preview.image);
            store.finish(key.clone(), Ok(preview), cx);
            image
        }));
        store.update(cx, |store, cx| {
            for (key, image) in keys.iter().zip(&images) {
                let (preview, _, _) = store.get(key.clone(), cx.entity_id(), cx);
                let preview = preview.expect("visible diagram must reuse its first raster");
                assert!(image.ptr_eq(&Arc::downgrade(&preview.image)));
            }
        });
        let painted = images[0].upgrade().unwrap();
        store.update(cx, |store, cx| {
            assert!(store.get(keys[2].clone(), cx.entity_id(), cx).0.is_some());
        });
        assert!(images[1].upgrade().is_none());
        drop(painted);
        store.update(cx, |store, cx| {
            assert!(store.get(keys[2].clone(), cx.entity_id(), cx).0.is_some());
        });
        assert!(images[0].upgrade().is_none());
    }

    #[gpui::test]
    fn revisiting_diagram_reuses_raster_and_other_theme_reserves_its_extent(cx: &mut gpui::TestAppContext) {
        let source: Arc<str> = Arc::from("graph TD\n A --> B");
        let store = cx.update(|cx| {
            let preview = MermaidPreview::render(&source, false, cx.svg_renderer()).unwrap();
            let store = mermaid_cache(cx);
            store.update(cx, |store, _| {
                store.remember_extent(source.clone(), preview.extent);
                store.bytes = MermaidStore::preview_bytes(&preview);
                store.entries.push_back(((source.clone(), false), MermaidSlot::Ready(preview), 0));
            });
            store
        });
        store.update(cx, |store, cx| {
            let owner = cx.entity_id();
            let (first, _, _) = store.get((source.clone(), false), owner, cx);
            let (second, _, _) = store.get((source.clone(), false), owner, cx);
            let first = first.unwrap();
            assert!(Arc::ptr_eq(&first.image, &second.unwrap().image));
            let (other_theme, reserved, failed) = store.get((source.clone(), true), owner, cx);
            assert!(other_theme.is_none() && !failed);
            let reserved = reserved.unwrap();
            assert_eq!(reserved.width, first.extent.width);
            assert_eq!(reserved.aspect, first.extent.aspect);
        });
    }
}
