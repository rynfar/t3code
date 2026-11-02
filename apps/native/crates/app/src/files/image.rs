//! The image preview (`WorkspaceImagePreview`, `PreviewImageContent.tsx`): a signed URL from
//! `assets.createUrl`, the bytes over HTTP with a size cap, and a bounded decode off the UI
//! thread (huge images are scaled down before they reach the GPU). The view fits the image to
//! the panel without enlarging it, under a header with its dimensions. One decoded image is
//! held at a time and released from the sprite atlas when it leaves.

use std::io::Cursor;
use std::sync::Arc;

use gpui::{AnyElement, App, ImageFormat, IntoElement, ObjectFit, ParentElement, RenderImage, Styled, StyledImage, div, img, prelude::*, px};
use gpui_component::{h_flex, v_flex};
use t3_core::files::{CREATE_ASSET_URL, ImageKind, absolute_path, decode_asset_url, image_asset_request};

use super::Root;
use crate::app_state::AppState;
use crate::theme::{hsla, palette};

const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Decoding refuses images wider or taller than this, and memory beyond `MAX_DECODE_ALLOC`.
const MAX_SOURCE_SIDE: u32 = 16_384;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
/// What reaches the GPU: larger images are scaled to fit this square.
const MAX_TEXTURE_SIDE: u32 = 4096;
const UNSUPPORTED: &str = "Unable to load workspace image.";

pub enum ImageState {
    Loading,
    Raster { image: Arc<RenderImage>, width: u32, height: u32 },
    Svg(Arc<gpui::Image>),
    Failed(String),
}

pub enum Decoded {
    Raster { image: Arc<RenderImage>, width: u32, height: u32 },
    Svg(Arc<gpui::Image>),
}

impl From<Result<Decoded, String>> for ImageState {
    fn from(result: Result<Decoded, String>) -> Self {
        match result {
            Ok(Decoded::Raster { image, width, height }) => ImageState::Raster { image, width, height },
            Ok(Decoded::Svg(image)) => ImageState::Svg(image),
            Err(error) => ImageState::Failed(error),
        }
    }
}

/// Mints the URL and downloads the bytes on the network runtime.
pub fn load(state: &AppState, root: &Root, path: &str, kind: ImageKind) -> Option<tokio::task::JoinHandle<Result<Vec<u8>, String>>> {
    let client = state.client_handle()?;
    let request = image_asset_request(&root.workspace.thread_id, &root.workspace.cwd, &absolute_path(path, &root.workspace.cwd));
    Some(state.runtime().spawn(async move {
        if kind == ImageKind::Unsupported {
            return Err(UNSUPPORTED.to_owned());
        }
        let value = client.request(CREATE_ASSET_URL, request).await.map_err(|_| UNSUPPORTED.to_owned())?;
        let url = decode_asset_url(&value)?;
        client.asset_bytes(&url, MAX_IMAGE_BYTES).await
    }))
}

/// Runs on the background executor.
pub fn decode(bytes: Vec<u8>, svg: bool) -> Result<Decoded, String> {
    if svg {
        return Ok(Decoded::Svg(Arc::new(gpui::Image::from_bytes(ImageFormat::Svg, bytes))));
    }
    let failed = |_| "Unable to load image.".to_owned();
    let mut reader = image::ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format().map_err(failed)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| "Unable to load image.".to_owned())?;
    let orientation = image::ImageDecoder::orientation(&mut decoder).map_err(|_| "Unable to load image.".to_owned())?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| "Unable to load image.".to_owned())?;
    decoded.apply_orientation(orientation);
    let (width, height) = (decoded.width(), decoded.height());
    if width.max(height) > MAX_TEXTURE_SIDE {
        decoded = decoded.thumbnail(MAX_TEXTURE_SIDE, MAX_TEXTURE_SIDE);
    }
    let mut pixels = decoded.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Decoded::Raster { image: Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])), width, height })
}

/// `toLocaleString` for a dimension.
fn grouped(value: u32) -> String {
    t3_core::files::group_thousands(u64::from(value))
}

impl ImageState {
    pub fn failed(&self) -> bool {
        matches!(self, ImageState::Failed(_))
    }

    /// Frees the decoded pixels and their GPU texture.
    pub fn release(self, cx: &mut App) {
        match self {
            ImageState::Raster { image, .. } => cx.drop_image(image, None),
            ImageState::Svg(image) => image.remove_asset(cx),
            ImageState::Loading | ImageState::Failed(_) => {}
        }
    }

    pub fn render(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let header = |text: String| {
            h_flex()
                .flex_none()
                .min_h(px(40.))
                .px(px(12.))
                .py(px(4.))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.6))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(text)
        };
        let frame = |image: gpui::Img, width: f32, height: f32| {
            div().id("file-image").flex_1().min_h_0().overflow_hidden().flex().items_center().justify_center().p(px(16.)).child(
                image.object_fit(ObjectFit::Contain).size_full().max_w(px(width)).max_h(px(height)),
            )
        };
        match self {
            ImageState::Loading => v_flex().flex_1().child(header("Loading image…".to_owned())).into_any_element(),
            ImageState::Failed(error) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_xs()
                .line_height(px(19.5))
                .text_color(hsla(p.destructive))
                .child(error.clone())
                .into_any_element(),
            ImageState::Raster { image, width, height } => v_flex()
                .flex_1()
                .min_h_0()
                .child(header(format!("{} × {}", grouped(*width), grouped(*height))))
                .child(frame(img(image.clone()), *width as f32, *height as f32))
                .into_any_element(),
            ImageState::Svg(image) => v_flex()
                .flex_1()
                .min_h_0()
                .child(header("SVG".to_owned()))
                .child(frame(img(image.clone()), 4096., 4096.))
                .into_any_element(),
        }
    }
}
