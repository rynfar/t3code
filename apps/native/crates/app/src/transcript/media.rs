//! Images in transcript rows: a user message's attachments (`TimelineUserMessageRow.tsx`, §7.5)
//! and images authored in markdown (`ChatMarkdownMedia.tsx`). Both draw from the bounded media
//! cache; linked Markdown images defer to their anchor. File rows port `chat/GuestMessageAttachments.tsx`.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, App, Context, Div, EntityId, FontWeight, Hsla, InteractiveElement, IntoElement, ObjectFit, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, hsla as gpui_hsla, img, px, relative, rems};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, markdown_ast as mdast};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::media::{AssetResource, ImageAttachment, MediaHints, MediaKind, MediaPlacement, MediaResolveOptions, MediaSource, VideoAttachment, resolve_media_source_with_options};
use t3_core::timeline::AnswerAttachment;

use crate::icons::Icon;
use crate::media::video::VideoPlayerOptions;
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
    images.iter().map(|image| ViewerItem::image(image.source(), image.name.clone())).collect()
}

fn video_gallery(videos: &[VideoAttachment]) -> Vec<ViewerItem> {
    videos.iter().map(|video| ViewerItem::video(video.source(), video.name.clone())).collect()
}

/// The video attachments of a user message (`MediaVideoPlayer.tsx`, `TimelineUserMessageRow.tsx`).
pub fn user_videos(
    message_id: &str,
    videos: &[VideoAttachment],
    placement: MediaPlacement,
    _grouped_previous: bool,
    _grouped_next: bool,
    cx: &mut Context<ThreadView>,
) -> AnyElement {
    let items = Arc::new(video_gallery(videos));
    let view = cx.entity().downgrade();
    let scope = cx.try_global::<media::Media>().and_then(|media| media.scope.clone());
    let (max_width, max_height) = if placement == MediaPlacement::InBubble { (210., 158.) } else { (360., 240.) };
    let cards: Vec<AnyElement> = videos
        .iter()
        .enumerate()
        .map(|(index, video)| {
            let items = items.clone();
            let view = view.clone();
            let options = VideoPlayerOptions {
                source: video.source(),
                name: video.name.clone().into(),
                revision: None,
                hints: MediaHints { width: None, height: None, explicit_video: true },
                modal: false,
                max_width,
                max_height,
                on_expand: Some(Arc::new(move |window, cx| {
                    let _ = view.update(cx, |view, cx| view.open_media(items.to_vec(), index, window, cx));
                })),
                on_open_external: None,
                on_close_modal: None,
            };
            crate::media::video::inline_video_player(format!("user-video:{message_id}:{index}"), options, scope.as_ref(), cx)
        })
        .collect();
    v_flex().gap(px(8.)).when(placement == MediaPlacement::InBubble, |el| el.mb(px(8.))).children(cards).into_any_element()
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

/// An authored markdown image, with its enclosing link and HTML size/tag hints if present.
#[derive(Clone, Debug)]
struct MarkdownImage {
    url: String,
    alt: String,
    link: Option<String>,
    hints: MediaHints,
}

const MEDIA_NODE: &str = "t3-media";
pub type MediaDocument = Arc<std::sync::RwLock<Arc<t3_core::media::MarkdownMediaDocument>>>;
pub fn media_document(text: Arc<str>) -> MediaDocument {
    Arc::new(std::sync::RwLock::new(Arc::new(t3_core::media::MarkdownMediaDocument::new(text))))
}
pub(super) fn media_document_with_inherited(parent: &MediaDocument, text: Arc<str>) -> MediaDocument {
    let child = parent.read().unwrap().with_inherited_definitions(text);
    Arc::new(std::sync::RwLock::new(Arc::new(child)))
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
    pub(super) fn selection_text(&self, document: &t3_core::media::MarkdownMediaDocument, parent: String, quote: &str, cx: &App) -> Option<String> {
        let selected: Vec<_> = self.0.lock().unwrap().iter().filter_map(|(&(start, run), selection)| {
            if document.prose_run_at(start, run) != Some(selection.source.as_str()) { return None; }
            selection.state.upgrade().map(|state| state.read(cx).selected_text()).filter(|text| !text.trim().is_empty()).map(|text| (start, run, text))
        }).collect();
        let mut participants: Vec<_> = selected.iter().map(|(_, _, text)| text.as_str()).collect();
        if !parent.trim().is_empty() { participants.push(&parent); }
        if participants.is_empty() || participants.join("\n") != quote { return None; }
        t3_core::assistant_citations::media_selection_text(document, &parent, &selected)
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

fn render_media_runs(node: &MarkdownNode, selections: &MediaSelections, gallery_doc: Option<&MediaDocument>, scope: Option<&MediaScope>, window: &mut Window, cx: &mut App) -> AnyElement {
    use t3_core::media::MarkdownMediaRun;
    let Some(block) = node.data::<MediaBlock>().cloned() else { return div().into_any_element() };
    let scope_key = format!("{}:{}", window.current_view(), block.start);
    let block_id = format!("media-block:{scope_key}");
    let standalone = block.runs.iter().all(|run| !matches!(run, MarkdownMediaRun::Prose(text) if !text.trim().is_empty()));
    let children = block.runs.into_iter().enumerate().filter_map(|(index, run)| match run {
        MarkdownMediaRun::Prose(text) if text.trim().is_empty() => None,
        MarkdownMediaRun::Prose(text) => Some(ProseElement { id: format!("{block_id}:prose:{index}").into(), text, heading: block.heading, selection: selections.clone(), key: (block.start, index) }.into_any_element()),
        MarkdownMediaRun::Image { url, alt, link, hints } => Some(markdown_image(&MarkdownImage { url, alt, link, hints }, &scope_key, index, gallery_doc, scope, cx)),
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

#[derive(Clone)]
pub(super) enum MediaScopeSource {
    Global,
    Shared(Arc<std::sync::Mutex<MediaScope>>),
    Override(Arc<std::sync::Mutex<Option<MediaScope>>>),
}

impl MediaScopeSource {
    fn resolve(&self, cx: &App) -> Option<MediaScope> {
        let local = match self {
            MediaScopeSource::Global => None,
            MediaScopeSource::Shared(scope) => Some(scope.lock().unwrap().clone()),
            MediaScopeSource::Override(scope) => scope.lock().unwrap().clone(),
        };
        local.or_else(|| cx.try_global::<media::Media>().and_then(|media| media.scope.clone()))
    }
}

pub fn markdown_extensions(document: MediaDocument, scope: Option<Arc<std::sync::Mutex<MediaScope>>>) -> MarkdownExtensions {
    let source = scope.map_or(MediaScopeSource::Global, MediaScopeSource::Shared);
    markdown_extensions_with_gallery(document.clone(), document, source, MediaSelections::default())
}

pub(super) fn markdown_extensions_with_selections(document: MediaDocument, scope: MediaScopeSource, selections: MediaSelections) -> MarkdownExtensions {
    markdown_extensions_with_gallery(document.clone(), document, scope, selections)
}

pub(super) fn markdown_extensions_with_gallery(
    parse_document: MediaDocument,
    gallery_document: MediaDocument,
    scope_source: MediaScopeSource,
    selections: MediaSelections,
) -> MarkdownExtensions {
    let parse_for_parser = parse_document;
    MarkdownExtensions::default().block_parser(move |node, context| {
        let document = parse_for_parser.read().unwrap().clone();
        parse_media_block(node, context, &document)
    }).block_renderer(MEDIA_NODE, move |node: &MarkdownNode, window: &mut Window, cx: &mut App| {
        let scope = scope_source.resolve(cx);
        render_media_runs(node, &selections, Some(&gallery_document), scope.as_ref(), window, cx)
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

fn resolve_with_scope(url: &str, scope: &MediaScope, explicit_video: bool) -> Option<t3_core::media::ResolvedMedia> {
    resolve_media_source_with_options(
        url,
        scope.thread_id.as_deref(),
        scope.workspace_root.as_deref(),
        true,
        MediaResolveOptions {
            explicit_video,
            source_control_media: scope.source_control_media,
            source_control_cwd: scope.workspace_root.as_deref(),
            source_control_repository: scope.source_control_media_repository.as_deref(),
        },
    )
}

fn open_gallery_item(gallery_doc: Option<&MediaDocument>, scope: &MediaScope, fallback: ViewerItem, window: &mut Window, cx: &mut App) {
    let mut items = Vec::new();
    if let Some(doc) = gallery_doc {
        let doc = doc.read().unwrap().clone();
        for (url, alt, hints) in doc.gallery_images() {
            let Some(resolved) = resolve_with_scope(&url, scope, hints.explicit_video) else { continue };
            let Some(source) = resolved.source else { continue };
            let name: SharedString = if alt.trim().is_empty() { resolved.name.into() } else { alt.into() };
            let item = match resolved.kind {
                MediaKind::Video => ViewerItem::video(source, name),
                MediaKind::Image => ViewerItem::image(source, name),
            };
            items.push(item);
        }
    }
    let index = items.iter().position(|item| item.source == fallback.source && item.kind == fallback.kind);
    let (items, index) = match index {
        Some(index) => (items, index),
        None => (vec![fallback], 0),
    };
    let _ = scope.viewer.update(cx, |viewer, cx| viewer.open_with_scope(items, index, Some(scope.clone()), window, cx));
}

fn markdown_image(image: &MarkdownImage, scope_key: &str, index: usize, gallery_doc: Option<&MediaDocument>, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let content = markdown_image_content(image, scope_key, index, gallery_doc, scope, cx);
    let Some(link) = image.link.as_ref().filter(|link| t3_core::media::markdown_external_link(link)).cloned() else { return content };
    let viewer = scope.map(|scope| scope.viewer.clone());
    let link_scope = scope.cloned();
    div().id(SharedString::from(format!("md-link:{index}:{}", image.url)))
        .cursor_pointer().child(content).on_click(move |event, window, cx| {
            use t3_core::media::{MarkdownImageAction, markdown_image_action};
            cx.stop_propagation();
            let modifiers = event.modifiers();
            match markdown_image_action(Some(&link), modifiers.platform || modifiers.control || modifiers.shift || modifiers.alt) {
                MarkdownImageAction::PreviewLink(url) if viewer.is_some() => {
                    let resolved = link_scope.as_ref().and_then(|scope| resolve_with_scope(url, scope, false));
                    let item = match resolved {
                        Some(resolved) if resolved.kind == MediaKind::Video => {
                            ViewerItem::video(resolved.source.unwrap_or_else(|| MediaSource::Direct(url.to_owned())), resolved.name)
                        }
                        Some(resolved) => {
                            ViewerItem::image(resolved.source.unwrap_or_else(|| MediaSource::Direct(url.to_owned())), resolved.name)
                        }
                        None if t3_core::media::is_workspace_video_preview_path(url) => ViewerItem::video(MediaSource::Direct(url.to_owned()), url.to_owned()),
                        None => ViewerItem::image(MediaSource::Direct(url.to_owned()), url.to_owned()),
                    };
                    let _ = viewer.as_ref().unwrap().update(cx, |viewer, cx| viewer.open(vec![item], 0, window, cx));
                }
                MarkdownImageAction::OpenLink(url) | MarkdownImageAction::PreviewLink(url) => cx.open_url(url),
                _ => {},
            }
        }).into_any_element()
}

/// A markdown image or video (`ChatMarkdownMedia.tsx`): at most 480 wide (or authored HTML width/height),
/// its own aspect, radius 8, clicking opens the full message gallery in `MediaViewer`.
fn markdown_image_content(image: &MarkdownImage, scope_key: &str, index: usize, gallery_doc: Option<&MediaDocument>, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let Some(scope) = scope else { return unavailable("Image", &image.alt, cx) };
    let Some(resolved) = resolve_with_scope(&image.url, scope, image.hints.explicit_video) else {
        return unavailable(if image.hints.explicit_video { "Video unavailable" } else { "Image unavailable" }, &image.alt, cx);
    };
    let name: SharedString = if image.alt.trim().is_empty() { resolved.name.into() } else { image.alt.clone().into() };
    if resolved.kind == MediaKind::Video {
        let Some(source) = resolved.source else {
            return unavailable("Video unavailable", &image.alt, cx);
        };
        let expand_item = ViewerItem::video(source.clone(), name.clone());
        let expand_gallery = gallery_doc.cloned();
        let expand_scope = scope.clone();
        let on_expand = (image.link.is_none()).then(|| {
            Arc::new(move |window: &mut Window, cx: &mut App| {
                open_gallery_item(expand_gallery.as_ref(), &expand_scope, expand_item.clone(), window, cx);
            }) as Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>
        });
        let options = VideoPlayerOptions {
            source: source.clone(),
            name,
            revision: media::revision_for(&source, scope).map(str::to_owned),
            hints: image.hints,
            modal: false,
            max_width: 480.,
            max_height: 360.,
            on_expand,
            on_open_external: None,
            on_close_modal: None,
        };
        return crate::media::video::inline_video_player(md_video_key(scope_key, index, &image.url), options, Some(scope), cx);
    }
    let Some(source) = resolved.source else { return unavailable("Image unavailable", &image.alt, cx) };
    let id = SharedString::from(md_image_id(scope_key, index, &image.url));
    framed_image(id, source, name, ImageFrame { alt: &image.alt, max_height: 480., preview: image.link.is_none(), hints: image.hints, gallery: gallery_doc.cloned() }, scope, cx)
}

/// `ChatMarkdownAssetImage` for an image a tool returned: within 480 wide and 256 tall
/// (`maxHeightRem={16}`), opening the viewer on click.
pub fn tool_output_image(id: SharedString, source: MediaSource, alt: &str, cx: &mut App) -> AnyElement {
    let scope = cx.try_global::<media::Media>().and_then(|media| media.scope.clone());
    let Some(scope) = scope else { return unavailable("Image", alt, cx) };
    framed_image(id, source, SharedString::from(alt.to_owned()), ImageFrame { alt, max_height: 256., preview: true, hints: MediaHints::default(), gallery: None }, &scope, cx)
}

struct ImageFrame<'a> {
    alt: &'a str,
    max_height: f32,
    preview: bool,
    hints: MediaHints,
    gallery: Option<MediaDocument>,
}

fn image_frame_size(natural: Option<(u32, u32)>, hints: MediaHints, max_height: f32) -> (f32, f32) {
    let cap_w = hints.width.map(|w| (w as f32).clamp(16., 480.)).unwrap_or(480.);
    let cap_h = hints.height.map(|h| (h as f32).clamp(16., max_height.max(16.))).unwrap_or(max_height);
    if let Some((width, height)) = natural.filter(|(w, h)| *w > 0 && *h > 0) {
        let fit = (cap_w / width as f32).min(cap_h / height as f32).min(1.);
        return ((width as f32 * fit).min(cap_w), (height as f32 * fit).min(cap_h));
    }
    match (hints.width.filter(|&w| w > 0), hints.height.filter(|&h| h > 0)) {
        (Some(w), Some(h)) => {
            let fit = (cap_w / w as f32).min(cap_h / h as f32).min(1.);
            (w as f32 * fit, h as f32 * fit)
        }
        (Some(w), None) => {
            let width = (w as f32).min(cap_w);
            (width, (width * 9. / 16.).min(cap_h))
        }
        (None, Some(h)) => {
            let height = (h as f32).min(cap_h);
            ((height * 16. / 9.).min(cap_w), height)
        }
        (None, None) => {
            let height = cap_h.min(270.);
            ((height * 16. / 9.).min(cap_w), height)
        }
    }
}

/// An image at its own aspect, at most 480 wide and `max_height` tall, radius 8 with a 40%
/// border. A 16:9 frame holds its place until the size is known.
fn framed_image(id: SharedString, source: MediaSource, name: SharedString, ImageFrame { alt, max_height, preview, hints, gallery }: ImageFrame<'_>, scope: &MediaScope, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    if matches!(media::peek(&source, cx), ImageState::Failed(_)) {
        return div().id(id).relative().child(unavailable("Image unavailable", alt, cx)).child(media::observe_image(source, scope.clone())).into_any_element();
    }

    let (width, height) = image_frame_size(media::dimensions(&source, cx), hints, max_height);
    let item = ViewerItem::image(source.clone(), name);
    let click_scope = scope.clone();
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
            open_gallery_item(gallery.as_ref(), &click_scope, item.clone(), window, cx);
        }));
    frame.child(pixels).child(div().absolute().inset_0().rounded(px(8.)).border_1().border_color(hsla(p.border).opacity(0.4))).into_any_element()
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

fn md_video_key(scope_key: &str, index: usize, url: &str) -> String {
    format!("md-video:{scope_key}:{index}:{url}")
}

fn md_image_id(scope_key: &str, index: usize, url: &str) -> String {
    format!("md-media:{scope_key}:{index}:{url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_frame_size_respects_authored_width_height_and_caps() {
        assert_eq!(image_frame_size(None, MediaHints { width: Some(320), height: Some(180), explicit_video: false }, 480.), (320., 180.));
        assert_eq!(image_frame_size(None, MediaHints { width: Some(960), height: Some(720), explicit_video: false }, 480.), (480., 360.));
        assert_eq!(image_frame_size(Some((1600, 900)), MediaHints { width: Some(240), height: None, explicit_video: false }, 480.), (240., 135.));
    }

    #[test]
    fn resolve_with_scope_routes_source_control_media_and_explicit_video() {
        let mut scope = MediaScope {
            owner: EntityId::from(1u64),
            thread_id: Some("thread-1".into()),
            workspace_root: Some("/repo".into()),
            workspace_mutation_id: None,
            source_control_media: true,
            source_control_media_repository: Some("org/repo".into()),
            viewer: gpui::WeakEntity::new_invalid(),
        };
        let resolved = resolve_with_scope("https://github.com/org/repo/assets/123/abc", &scope, false).unwrap();
        assert_eq!(
            resolved.source,
            Some(MediaSource::Asset(AssetResource::SourceControlMedia {
                cwd: "/repo".into(),
                url: "https://github.com/org/repo/assets/123/abc".into(),
                repository: Some("org/repo".into()),
            }))
        );
        let explicit = resolve_with_scope("https://example.com/media?id=99", &scope, true).unwrap();
        assert_eq!(explicit.kind, MediaKind::Video);

        scope.source_control_media = false;
        let plain = resolve_with_scope("https://github.com/user-attachments/assets/abc", &scope, false).unwrap();
        assert_eq!(plain.kind, MediaKind::Image);
        assert_eq!(plain.source, Some(MediaSource::Direct("https://github.com/user-attachments/assets/abc".into())));
    }

    #[test]
    fn block_media_keys_isolate_duplicate_urls_across_blocks() {
        let same_url = "https://example.com/demo.mp4";
        assert_ne!(md_video_key("12:0", 0, same_url), md_video_key("12:48", 0, same_url));
        assert_ne!(md_video_key("12:0", 0, same_url), md_video_key("13:0", 0, same_url));
        assert_ne!(md_image_id("12:0", 0, same_url), md_image_id("12:48", 0, same_url));
    }
}
