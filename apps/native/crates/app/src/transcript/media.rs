//! Images in transcript rows: a user message's attachments (`TimelineUserMessageRow.tsx`, §7.5)
//! and images authored in markdown (`ChatMarkdownMedia.tsx`). Both draw from the bounded media
//! cache; linked Markdown images defer to their anchor. File rows port `chat/GuestMessageAttachments.tsx`.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, App, Context, Div, EntityId, FontWeight, Hsla, InteractiveElement, IntoElement, ObjectFit, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, hsla as gpui_hsla, img, px, relative, rems};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, markdown_ast as mdast};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::media::{AssetResource, ImageAttachment, MediaKind, MediaPlacement, MediaSource, resolve_media_source};
use t3_core::timeline::AnswerAttachment;

use crate::icons::Icon;
use crate::media::{self, ImageState, MediaScope, Variant, ViewerItem};
use crate::theme::{hsla, palette, rem};
use crate::thread_view::ThreadView;

/// One corner radius per corner: top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy)]
struct Corners(f32, f32, f32, f32);

impl Corners {
    fn all(radius: f32) -> Self {
        Corners(radius, radius, radius, radius)
    }

    fn only(self, tl: bool, tr: bool, br: bool, bl: bool) -> Self {
        let pick = |on: bool, radius: f32| if on { radius } else { 0. };
        Corners(pick(tl, self.0), pick(tr, self.1), pick(br, self.2), pick(bl, self.3))
    }
}

fn rounded<E: Styled>(element: E, corners: Corners) -> E {
    element.rounded_tl(px(corners.0)).rounded_tr(px(corners.1)).rounded_br(px(corners.2)).rounded_bl(px(corners.3))
}

/// The 1px inset ring around standalone media (black 9%, dark white 12%).
fn edge_color(cx: &App) -> Hsla {
    if cx.theme().is_dark() { gpui_hsla(0., 0., 1., 0.12) } else { gpui_hsla(0., 0., 0., 0.09) }
}

fn ring(corners: Corners, cx: &App) -> Div {
    rounded(div().absolute().inset_0().border_1().border_color(edge_color(cx)), corners)
}

/// A slot's size, corners and fill before its image arrives.
#[derive(Clone, Copy)]
struct Slot {
    width: f32,
    height: f32,
    corners: Corners,
    fill: Hsla,
}

/// One image slot: the picture once decoded, its name while it cannot load.
fn image_cell(image: &ImageAttachment, Slot { width, height, corners, fill }: Slot, owner: EntityId, cx: &mut App) -> Div {
    let (source, name) = (image.source(), SharedString::from(image.name.clone()));
    let state = media::image(&source, Variant::Thumb, owner, cx);
    let p = palette(cx);
    let cell = rounded(div().relative().flex_none().w(px(width)).h(px(height)).max_w_full().overflow_hidden().bg(fill), corners);
    let cell = if let Some(scope) = cx.try_global::<media::Media>().and_then(|media| media.scope.clone()) {
        cell.child(media::observe_image(source.clone(), scope))
    } else { cell };
    match state {
        ImageState::Ready(image) => cell.child(rounded(img(image).size_full().object_fit(ObjectFit::Cover), corners)),
        ImageState::Loading => cell,
        ImageState::Failed(_) => cell.flex().items_center().justify_center().px(px(8.)).py(px(12.)).child(
            div().text_size(px(10.)).text_color(hsla(p.muted_foreground).opacity(0.7)).text_center().child(name),
        ),
    }
}

/// A lone image keeps its own shape: 192 tall, as wide as its aspect allows (min 96).
fn lone_width(source: &MediaSource, cx: &App) -> f32 {
    match media::dimensions(source, cx) {
        Some((width, height)) if height > 0 => (192. * width as f32 / height as f32).max(96.),
        _ => 256.,
    }
}

fn gallery(images: &[ImageAttachment]) -> Vec<ViewerItem> {
    images.iter().map(|image| ViewerItem { source: image.source(), name: image.name.clone().into() }).collect()
}

/// The media block of a user message, laid out as the web lays it out: a small 4:3 grid inside
/// guest and agent bubbles, otherwise a lone image or a collage split by hairlines.
pub fn user_media(
    message_id: &str,
    images: &[ImageAttachment],
    placement: MediaPlacement,
    grouped_previous: bool,
    grouped_next: bool,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    let owner = cx.entity_id();
    let p = palette(cx);
    let items = Arc::new(gallery(images));
    let view = cx.entity().downgrade();
    let clickable = |index: usize, cell: Div, _: &mut Context<ThreadView>| {
        let (items, view) = (items.clone(), view.clone());
        cell.id(SharedString::from(format!("media:{message_id}:{index}")))
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |view, cx| view.open_media(items.to_vec(), index, window, cx));
            })
            .into_any_element()
    };
    if placement == MediaPlacement::InBubble {
        // `max-w-[210px] grid-cols-2 gap-2`, 4:3 cells with a hairline border.
        let (width, height) = (101., 75.75);
        let fill = hsla(p.background).opacity(0.7);
        let cells: Vec<AnyElement> = images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let cell = image_cell(image, Slot { width, height, corners: Corners::all(8.), fill }, owner, cx)
                    .child(rounded(div().absolute().inset_0().border_1().border_color(hsla(p.border).opacity(0.8)), Corners::all(8.)));
                clickable(index, cell, cx)
            })
            .collect();
        return h_flex().flex_wrap().max_w(px(210.)).gap(px(8.)).mb(px(8.)).children(cells).into_any_element();
    }
    // Corners touching the bubble or a neighbouring message tighten, like the bubble's.
    let corners = Corners(16., if grouped_previous { 6. } else { 16. }, if placement == MediaPlacement::AboveText || grouped_next { 6. } else { 16. }, 16.);
    if let [image] = images {
        let source = image.source();
        let width = lone_width(&source, cx);
        let cell = image_cell(image, Slot { width, height: 192., corners, fill: hsla(p.background).opacity(0.7) }, owner, cx).child(ring(corners, cx));
        return clickable(0, cell, cx);
    }
    // A collage: 320 wide, 1px hairlines in the ring's color, outer corners rounded per cell.
    let fill = hsla(p.background);
    let half = (320. - 1.) / 2.;
    let cell = |index: usize, width: f32, height: f32, round: Corners, cx: &mut Context<ThreadView>| {
        let element = image_cell(&images[index], Slot { width, height, corners: round, fill }, owner, cx);
        clickable(index, element, cx)
    };
    let body: AnyElement = match images.len() {
        2 => h_flex()
            .gap(px(1.))
            .child(cell(0, half, 192., corners.only(true, false, false, true), cx))
            .child(cell(1, half, 192., corners.only(false, true, true, false), cx))
            .into_any_element(),
        3 => {
            let (wide, narrow) = (319. * 3. / 5., 319. * 2. / 5.);
            h_flex()
                .gap(px(1.))
                .child(cell(0, wide, 192., corners.only(true, false, false, true), cx))
                .child(
                    v_flex()
                        .gap(px(1.))
                        .child(cell(1, narrow, 95.5, corners.only(false, true, false, false), cx))
                        .child(cell(2, narrow, 95.5, corners.only(false, false, true, false), cx)),
                )
                .into_any_element()
        }
        count => {
            let rows = count.div_ceil(2);
            let mut column = v_flex().gap(px(1.));
            for row in 0..rows {
                let (first, last) = (row == 0, row + 1 == rows);
                let left = row * 2;
                column = if left + 1 < count {
                    column.child(
                        h_flex()
                            .gap(px(1.))
                            .child(cell(left, half, 96., corners.only(first, false, false, last), cx))
                            .child(cell(left + 1, half, 96., corners.only(false, first, last, false), cx)),
                    )
                } else {
                    column.child(cell(left, 320., 96., corners.only(false, false, true, true), cx))
                };
            }
            column.into_any_element()
        }
    };
    rounded(div().relative().w(px(320.)).max_w_full().bg(edge_color(cx)), corners).child(body).child(ring(corners, cx)).into_any_element()
}

/// An authored markdown image, with its enclosing link if present.
#[derive(Clone, Debug)]
struct MarkdownImage {
    url: String,
    alt: String,
    link: Option<String>,
}

const MEDIA_NODE: &str = "t3-media";
pub type MediaDocument = Arc<std::sync::RwLock<Arc<t3_core::media::MarkdownMediaDocument>>>;
pub fn media_document(text: Arc<str>) -> MediaDocument {
    Arc::new(std::sync::RwLock::new(Arc::new(t3_core::media::MarkdownMediaDocument::new(text))))
}
#[derive(Clone, Default)]
pub(super) struct MediaSelections(Arc<std::sync::Mutex<std::collections::BTreeMap<(usize, usize), ProseSelection>>>);
struct ProseSelection { state: gpui::WeakEntity<TextViewState>, source: String }
impl MediaSelections {
    pub(super) fn register(&self, key: (usize, usize), state: &gpui::Entity<TextViewState>, source: &str) {
        let mut states = self.0.lock().unwrap();
        if states.get(&key).is_some_and(|selection| selection.state.entity_id() == state.entity_id() && selection.source == source) { return; }
        states.retain(|_, selection| selection.state.upgrade().is_some());
        states.insert(key, ProseSelection { state: state.downgrade(), source: source.to_owned() });
    }
    pub(super) fn matches_quote(&self, document: &t3_core::media::MarkdownMediaDocument, quote: &str, cx: &App) -> bool {
        let selected: Vec<_> = self.0.lock().unwrap().iter().filter_map(|(&(start, run), selection)| {
            if document.prose_run_at(start, run) != Some(selection.source.as_str()) { return None; }
            selection.state.upgrade().map(|state| state.read(cx).selected_text()).filter(|text| !text.trim().is_empty())
        }).collect();
        selected.iter().any(|text| text == quote) || (!selected.is_empty() && selected.join("\n") == quote)
    }
}

#[derive(Clone)]
struct MediaBlock { start: usize, heading: Option<u8>, paragraph_gap: bool, runs: Vec<t3_core::media::MarkdownMediaRun> }
fn parse_media_block(node: &mdast::Node, context: &MarkdownParseContext, document: &t3_core::media::MarkdownMediaDocument) -> Option<MarkdownNode> {
    let runs = document.runs_at(node, context.offset());
    if runs.is_empty() { return None; }
    let start = context.offset() + node.position()?.start.offset;
    let heading = match node { mdast::Node::Heading(heading) => Some(heading.depth), _ => None };
    Some(MarkdownNode::new(MEDIA_NODE, MediaBlock { start, heading, paragraph_gap: document.heading_has_paragraph_gap(node, context.offset()), runs }).text(node.to_string()).markdown(context.node_source(node).unwrap_or_default().to_owned()))
}

fn render_media_runs(node: &MarkdownNode, selections: &MediaSelections, scope: Option<&MediaScope>, window: &mut Window, cx: &mut App) -> AnyElement {
    use t3_core::media::MarkdownMediaRun;
    let Some(block) = node.data::<MediaBlock>().cloned() else { return div().into_any_element() };
    let block_id = format!("media-block:{}:{}", window.current_view(), block.start);
    let standalone = block.runs.iter().all(|run| !matches!(run, MarkdownMediaRun::Prose(text) if !text.trim().is_empty()));
    let children = block.runs.into_iter().enumerate().filter_map(|(index, run)| match run {
        MarkdownMediaRun::Prose(text) if text.trim().is_empty() => None,
        MarkdownMediaRun::Prose(text) => Some(ProseElement { id: format!("{block_id}:prose:{index}").into(), text, heading: block.heading, selection: selections.clone(), key: (block.start, index) }.into_any_element()),
        MarkdownMediaRun::Image { url, alt, link } => Some(markdown_image(&MarkdownImage { url, alt, link }, index, scope, cx)),
    }).collect::<Vec<_>>();
    let content = if standalone { v_flex().gap(px(8.)).items_start().children(children).into_any_element() }
        else { h_flex().flex_wrap().gap(px(4.)).items_end().children(children).into_any_element() };
    div().id(SharedString::from(block_id))
        .when(block.heading.is_some(), |element| {
            // Custom blocks receive paragraph padding outside this renderer; cancel only that padding.
            element.pb(rems(0.3)).when(block.paragraph_gap, |element| element.mb(rems(-super::markdown::PARAGRAPH_GAP)))
        }).child(content).into_any_element()
}

struct ProseElement { id: SharedString, text: String, heading: Option<u8>, selection: MediaSelections, key: (usize, usize) }
impl IntoElement for ProseElement { type Element = Self; fn into_element(self) -> Self { self } }
impl gpui::Element for ProseElement {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> { Some(self.id.clone().into()) }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (gpui::LayoutId, AnyElement) {
        let state = window.use_keyed_state("state", cx, |_, cx| TextViewState::markdown(&self.text, cx));
        state.update(cx, |state, cx| state.set_text(&self.text, cx));
        self.selection.register(self.key, &state, &self.text);
        #[cfg(feature = "snapshot")]
        crate::snapshot::record_media_prose(&state, &self.text, cx);
        let mut child = div().min_w_0().when_some(self.heading, |element, level| {
            // Match the pinned TextView heading wrapper while keeping image bounds independent.
            element.text_size(super::markdown::heading_font_size(level)).font_weight(match level {
                1 => FontWeight::BOLD,
                6 => FontWeight::MEDIUM,
                _ => FontWeight::SEMIBOLD,
            })
        }).child(super::markdown::style_markdown(TextView::new(&state), cx)).into_any_element();
        (child.request_layout(window, cx), child)
    }
    fn prepaint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, child: &mut AnyElement, window: &mut Window, cx: &mut App) { child.prepaint(window, cx); }
    fn paint(&mut self, _: Option<&gpui::GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: gpui::Bounds<gpui::Pixels>, child: &mut AnyElement, _: &mut (), window: &mut Window, cx: &mut App) { child.paint(window, cx); }
}

pub fn markdown_extensions(document: MediaDocument, scope: Option<Arc<std::sync::Mutex<MediaScope>>>) -> MarkdownExtensions {
    markdown_extensions_with_selections(document, scope, MediaSelections::default())
}

pub(super) fn markdown_extensions_with_selections(document: MediaDocument, scope: Option<Arc<std::sync::Mutex<MediaScope>>>, selections: MediaSelections) -> MarkdownExtensions {
    MarkdownExtensions::default().block_parser(move |node, context| {
        let document = document.read().unwrap().clone();
        parse_media_block(node, context, &document)
    }).block_renderer(MEDIA_NODE, move |node: &MarkdownNode, window: &mut Window, cx: &mut App| {
        let scope = scope.as_ref().map(|scope| scope.lock().unwrap().clone())
            .or_else(|| cx.try_global::<media::Media>().and_then(|media| media.scope.clone()));
        render_media_runs(node, &selections, scope.as_ref(), window, cx)
    })
}

/// `ChatMarkdownImageFallback`: a small chip naming what could not be shown.
fn unavailable(label: &str, alt: &str, cx: &App) -> AnyElement {
    let p = palette(cx);
    let text: Vec<&str> = [label, alt].into_iter().filter(|part| !part.is_empty()).collect();
    h_flex()
        .gap(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(hsla(p.border).opacity(0.4))
        .bg(hsla(p.muted).opacity(0.4))
        .px(px(8.))
        .py(px(4.))
        .text_xs()
        .text_color(hsla(p.muted_foreground))
        .child(Icon::TriangleAlert.render(px(14.), hsla(p.muted_foreground)))
        .child(text.join(" · "))
        .into_any_element()
}

fn markdown_image(image: &MarkdownImage, index: usize, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let content = markdown_image_content(image, index, scope, cx);
    let Some(link) = image.link.as_ref().filter(|link| t3_core::media::markdown_external_link(link)).cloned() else { return content };
    let viewer = scope.map(|scope| scope.viewer.clone());
    div().id(SharedString::from(format!("md-link:{index}:{}", image.url)))
        .cursor_pointer().child(content).on_click(move |event, window, cx| {
            use t3_core::media::{MarkdownImageAction, markdown_image_action};
            cx.stop_propagation();
            let modifiers = event.modifiers();
            match markdown_image_action(Some(&link), modifiers.platform || modifiers.control || modifiers.shift || modifiers.alt) {
                MarkdownImageAction::PreviewLink(url) if viewer.is_some() => {
                    let item = ViewerItem { source: MediaSource::Direct(url.to_owned()), name: url.to_owned().into() };
                    let _ = viewer.as_ref().unwrap().update(cx, |viewer, cx| viewer.open(vec![item], 0, window, cx));
                }
                MarkdownImageAction::OpenLink(url) | MarkdownImageAction::PreviewLink(url) => cx.open_url(url),
                _ => {},
            }
        }).into_any_element()
}

/// A markdown image: at most 480 square, its own aspect, radius 8 with a 40% border. A 16:9
/// frame holds its place until the size is known.
fn markdown_image_content(image: &MarkdownImage, index: usize, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let Some(scope) = scope else { return unavailable("Image", &image.alt, cx) };
    let Some(resolved) = resolve_media_source(&image.url, scope.thread_id.as_deref(), scope.workspace_root.as_deref(), true) else {
        return unavailable("Image unavailable", &image.alt, cx);
    };
    if resolved.kind == MediaKind::Video {
        let p = palette(cx);
        let source = resolved.source;
        let name = if image.alt.is_empty() { resolved.name } else { image.alt.clone() };
        return v_flex().w(rem(480.)).max_w_full().min_h(rem(112.)).items_center().justify_center().gap(rem(12.)).p(rem(16.)).rounded(rem(8.)).border_1().border_color(hsla(p.border).opacity(0.4)).bg(hsla(p.muted).opacity(0.4))
            .child(unavailable("Video unavailable", &name, cx))
            .children(source.filter(|_| image.link.is_none()).filter(|source| match source { MediaSource::Asset(_) => true, MediaSource::Direct(url) => url.starts_with("http://") || url.starts_with("https://") }).map(|source| crate::right_panel::Button::new(SharedString::from(format!("video-open:{index}"))).variant(crate::right_panel::Variant::Secondary).size(crate::right_panel::Size::Xs).label(if matches!(&source, MediaSource::Direct(_)) { "Open original" } else { "Open in browser" }).on_click(move |_, _, cx| match &source {
                MediaSource::Asset(resource) => media::open_asset(resource, cx),
                MediaSource::Direct(url) if url.starts_with("http://") || url.starts_with("https://") => cx.open_url(url),
                _ => {},
            })))
            .into_any_element();
    }
    let Some(source) = resolved.source else { return unavailable("Image unavailable", &image.alt, cx) };
    let name: SharedString = if image.alt.trim().is_empty() { resolved.name.into() } else { image.alt.clone().into() };
    let id = SharedString::from(format!("md-media:{index}:{}", image.url));
    framed_image(id, source, name, ImageFrame { alt: &image.alt, max_height: 480., preview: image.link.is_none() }, scope, cx)
}

/// `ChatMarkdownAssetImage` for an image a tool returned: within 480 wide and 256 tall
/// (`maxHeightRem={16}`), opening the viewer on click.
pub fn tool_output_image(id: SharedString, source: MediaSource, alt: &str, cx: &mut App) -> AnyElement {
    let scope = cx.try_global::<media::Media>().and_then(|media| media.scope.clone());
    let Some(scope) = scope else { return unavailable("Image", alt, cx) };
    framed_image(id, source, SharedString::from(alt.to_owned()), ImageFrame { alt, max_height: 256., preview: true }, &scope, cx)
}

struct ImageFrame<'a> { alt: &'a str, max_height: f32, preview: bool }

/// An image at its own aspect, at most 480 wide and `max_height` tall, radius 8 with a 40%
/// border. A 16:9 frame holds its place until the size is known.
fn framed_image(id: SharedString, source: MediaSource, name: SharedString, ImageFrame { alt, max_height, preview }: ImageFrame<'_>, scope: &MediaScope, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    if matches!(media::peek(&source, cx), ImageState::Failed(_)) {
        return div().id(id).relative().child(unavailable("Image unavailable", alt, cx)).child(media::observe_image(source, scope.clone())).into_any_element();
    }

    let (width, height) = match media::dimensions(&source, cx) {
        Some((width, height)) => {
            let fit = (480. / width.max(1) as f32).min(max_height / height.max(1) as f32).min(1.);
            (width as f32 * fit, height as f32 * fit)
        }
        None => {
            let height = max_height.min(270.);
            (height * 16. / 9., height)
        }
    };
    let viewer = scope.viewer.clone();
    let item = ViewerItem { source: source.clone(), name };
    let pixels = media::scoped_image(&source, scope);
    let frame = div()
        .id(id)
        .relative()
        .w(px(width))
        .max_w(relative(1.))
        .h(px(height))
        .rounded(px(8.))
        .overflow_hidden()
        .bg(hsla(p.muted).opacity(0.6))
        .when(preview, |frame| frame.cursor_pointer().on_click(move |_, window, cx| {
            cx.stop_propagation();
            let item = item.clone();
            let _ = viewer.update(cx, |viewer, cx| viewer.open(vec![item], 0, window, cx));
        }));
    frame.child(pixels).child(div().absolute().inset_0().rounded(px(8.)).border_1().border_color(hsla(p.border).opacity(0.4))).into_any_element()
}

pub fn other_files(names: &[t3_core::media::FileAttachment], cx: &App) -> Option<AnyElement> {
    if names.is_empty() {
        return None;
    }
    let p = palette(cx);
    Some(
        v_flex()
            .gap(px(4.))
            .mb(px(8.))
            .children(names.iter().map(|name| {
                h_flex().min_w_0().gap(px(8.)).py(px(4.)).text_size(px(14.)).child(Icon::File.render(px(16.), hsla(p.muted_foreground))).child(div().min_w_0().truncate().child(name.name.clone())).when_some(name.size_bytes, |row, bytes| row.child(div().flex_shrink_0().text_xs().text_color(hsla(p.muted_foreground)).child(t3_core::composer::attachments::format_size(bytes))))
            }))
            .into_any_element(),
    )
}

/// A file given with an answer (`QuestionAnswerHistory`): an image as a thumbnail at most 128 by
/// 80 once it loads, anything else by its underlined name. Either opens the file in the browser.
pub fn answer_attachment(id: SharedString, attachment: &AnswerAttachment, cx: &mut App) -> AnyElement {
    let resource = AssetResource::Attachment { attachment_id: attachment.id.clone() };
    let owner = cx.try_global::<media::Media>().and_then(|media| media.scope.as_ref().map(|scope| scope.owner));
    let source = MediaSource::Asset(resource.clone());
    let image = match owner.filter(|_| attachment.image) {
        Some(owner) => match media::image(&source, Variant::Thumb, owner, cx) {
            ImageState::Ready(image) => Some(image),
            _ => None,
        },
        None => None,
    };
    let observer = cx.try_global::<media::Media>().and_then(|media| media.scope.clone()).filter(|_| attachment.image).map(|scope| media::observe_image(source.clone(), scope));
    let frame = div().id(id).relative().children(observer).flex_none().cursor_pointer().on_click(move |_, _, cx| {
        cx.stop_propagation();
        media::open_asset(&resource, cx);
    });
    match image {
        Some(image) => {
            let width = media::dimensions(&source, cx).filter(|(_, height)| *height > 0).map_or(80., |(width, height)| (80. * width as f32 / height as f32).min(128.));
            frame.h(rem(80.)).w(rem(width)).child(img(image).size_full().rounded(px(4.)).object_fit(ObjectFit::Contain)).into_any_element()
        }
        None => frame.underline().child(SharedString::from(attachment.name.clone())).into_any_element(),
    }
}
