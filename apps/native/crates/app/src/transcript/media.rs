//! Images in transcript rows: a user message's attachments (`TimelineUserMessageRow.tsx`, §7.5)
//! and images authored in markdown (`ChatMarkdownMedia.tsx`). Both draw from the bounded media
//! cache and open the viewer on click.

use std::sync::{Arc, OnceLock};

use gpui::{AnyElement, App, Context, Div, EntityId, Hsla, InteractiveElement, IntoElement, ObjectFit, ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledImage, Window, div, hsla as gpui_hsla, img, px, relative};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, markdown_ast as mdast};
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

/// An image paragraph found while parsing markdown.
#[derive(Clone, Debug)]
struct MarkdownImage {
    url: String,
    alt: String,
}

const MEDIA_NODE: &str = "t3-media";

/// A paragraph made only of images (and whitespace) draws as media; images inside running text
/// stay with gpui-component, which shows their alt text.
fn parse_media_paragraph(node: &mdast::Node, context: &MarkdownParseContext) -> Option<MarkdownNode> {
    let mdast::Node::Paragraph(paragraph) = node else { return None };
    let mut images = Vec::new();
    for child in &paragraph.children {
        match child {
            mdast::Node::Image(image) => images.push(MarkdownImage { url: image.url.clone(), alt: image.alt.clone() }),
            mdast::Node::Text(text) if text.value.trim().is_empty() => {}
            mdast::Node::Break(_) => {}
            _ => return None,
        }
    }
    if images.is_empty() {
        return None;
    }
    let source = paragraph.position.as_ref().and_then(|position| context.source().get(position.start.offset..position.end.offset)).unwrap_or_default().to_owned();
    let text = images.iter().map(|image| image.alt.as_str()).collect::<Vec<_>>().join(" ");
    Some(MarkdownNode::new(MEDIA_NODE, images).text(text).markdown(source))
}

/// The extension registry markdown with images is parsed with. Built once: a new registry would
/// make every message reparse.
pub fn markdown_extensions() -> MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS
        .get_or_init(|| {
            MarkdownExtensions::default().block_parser(parse_media_paragraph).block_renderer(MEDIA_NODE, |node: &MarkdownNode, _: &mut Window, cx: &mut App| {
                let images = node.data::<Vec<MarkdownImage>>().cloned().unwrap_or_default();
                // Only the transcript's markdown store parses with these extensions, so the scope is
                // the transcript's; the block renders inside its text state's own view.
                let scope = cx.try_global::<media::Media>().and_then(|media| media.scope.clone());
                v_flex().gap(px(8.)).items_start().children(images.iter().enumerate().map(|(index, image)| markdown_image(image, index, scope.as_ref(), cx)))
            })
        })
        .clone()
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

/// A markdown image: at most 480 square, its own aspect, radius 8 with a 40% border. A 16:9
/// frame holds its place until the size is known.
fn markdown_image(image: &MarkdownImage, index: usize, scope: Option<&MediaScope>, cx: &mut App) -> AnyElement {
    let Some(scope) = scope else { return unavailable("Image", &image.alt, cx) };
    let Some(resolved) = resolve_media_source(&image.url, scope.thread_id.as_deref(), scope.workspace_root.as_deref(), true) else {
        return unavailable("Image unavailable", &image.alt, cx);
    };
    if resolved.kind == MediaKind::Video {
        return unavailable("Video", if image.alt.is_empty() { &resolved.name } else { &image.alt }, cx);
    }
    let Some(source) = resolved.source else { return unavailable("Image unavailable", &image.alt, cx) };
    let name: SharedString = if image.alt.trim().is_empty() { resolved.name.into() } else { image.alt.clone().into() };
    let id = SharedString::from(format!("md-media:{index}:{}", image.url));
    framed_image(id, source, name, &image.alt, 480., scope, cx)
}

/// `ChatMarkdownAssetImage` for an image a tool returned: within 480 wide and 256 tall
/// (`maxHeightRem={16}`), opening the viewer on click.
pub fn tool_output_image(id: SharedString, source: MediaSource, alt: &str, cx: &mut App) -> AnyElement {
    let scope = cx.try_global::<media::Media>().and_then(|media| media.scope.clone());
    let Some(scope) = scope else { return unavailable("Image", alt, cx) };
    framed_image(id, source, SharedString::from(alt.to_owned()), alt, 256., &scope, cx)
}

/// An image at its own aspect, at most 480 wide and `max_height` tall, radius 8 with a 40%
/// border. A 16:9 frame holds its place until the size is known.
fn framed_image(id: SharedString, source: MediaSource, name: SharedString, alt: &str, max_height: f32, scope: &MediaScope, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let state = media::scoped_image(&source, Variant::Thumb, scope, cx);
    if let ImageState::Failed(_) = state {
        return unavailable("Image unavailable", alt, cx);
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
    let frame = div()
        .id(id)
        .relative()
        .w(px(width))
        .max_w(relative(1.))
        .h(px(height))
        .rounded(px(8.))
        .overflow_hidden()
        .bg(hsla(p.muted).opacity(0.6))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let item = item.clone();
            let _ = viewer.update(cx, |viewer, cx| viewer.open(vec![item], 0, window, cx));
        });
    let frame = match state {
        ImageState::Ready(image) => frame.child(img(image).size_full().rounded(px(8.)).object_fit(ObjectFit::Contain)),
        _ => frame,
    };
    frame.child(div().absolute().inset_0().rounded(px(8.)).border_1().border_color(hsla(p.border).opacity(0.4))).into_any_element()
}

pub fn other_files(names: &[String], cx: &App) -> Option<AnyElement> {
    if names.is_empty() {
        return None;
    }
    let p = palette(cx);
    Some(
        v_flex()
            .gap(px(4.))
            .mb(px(8.))
            .children(names.iter().map(|name| {
                h_flex().min_w_0().gap(px(8.)).text_size(px(14.)).child(Icon::Link.render(px(14.), hsla(p.muted_foreground))).child(div().min_w_0().truncate().child(name.clone()))
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
    let frame = div().id(id).flex_none().cursor_pointer().on_click(move |_, _, cx| {
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
