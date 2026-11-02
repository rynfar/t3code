//! Turns fetched bytes into GPU-ready pixels, off the UI thread. Raster formats decode with the
//! `image` crate GPUI already ships, downscaled to the size they draw at; anything else is tried
//! as SVG. Animated GIF and WebP show their first frame: an always-running animation would
//! repaint the transcript continuously.

use std::sync::Arc;

use gpui::{RenderImage, SvgRenderer};
use image::{DynamicImage, Frame};
use t3_core::media::fit_within;

pub struct Decoded {
    pub image: Arc<RenderImage>,
    /// The image's own size before downscaling.
    pub original: (u32, u32),
    /// Decoded pixel bytes, counted against the cache budget.
    pub bytes: usize,
}

const UNSUPPORTED: &str = "This image format can't be shown here.";

pub fn decode(bytes: &[u8], max_edge: u32, svg: &SvgRenderer) -> Result<Decoded, String> {
    match image::guess_format(bytes) {
        Ok(_) => raster(image::load_from_memory(bytes).map_err(|_| UNSUPPORTED.to_owned())?, max_edge),
        Err(_) => vector(bytes, svg),
    }
}

fn raster(image: DynamicImage, max_edge: u32) -> Result<Decoded, String> {
    let original = (image.width(), image.height());
    if original.0 == 0 || original.1 == 0 {
        return Err(UNSUPPORTED.into());
    }
    let (width, height) = fit_within(original.0, original.1, max_edge);
    let image = if (width, height) == original { image } else { image.thumbnail_exact(width, height) };
    let mut pixels = image.into_rgba8();
    // GPUI's atlas takes BGRA.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let bytes = pixels.len();
    Ok(Decoded { image: Arc::new(RenderImage::new(vec![Frame::new(pixels)])), original, bytes })
}

/// SVGs render at 2x so they stay sharp on Retina displays.
fn vector(bytes: &[u8], svg: &SvgRenderer) -> Result<Decoded, String> {
    let image = svg.render_single_frame(bytes, 2.0).map_err(|_| UNSUPPORTED.to_owned())?;
    let size = image.size(0);
    let (width, height) = (u32::try_from(size.width.0).unwrap_or(0), u32::try_from(size.height.0).unwrap_or(0));
    if width == 0 || height == 0 {
        return Err(UNSUPPORTED.into());
    }
    Ok(Decoded { image, original: (width.div_ceil(2), height.div_ceil(2)), bytes: width as usize * height as usize * 4 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_images_downscale_to_bgra() {
        let mut source = image::RgbaImage::new(4, 2);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgba([255, 0, 10, 255]);
        }
        let decoded = raster(DynamicImage::ImageRgba8(source), 2).unwrap();
        assert_eq!(decoded.original, (4, 2));
        assert_eq!((decoded.image.size(0).width.0, decoded.image.size(0).height.0), (2, 1));
        assert_eq!(decoded.bytes, 2 * 4);
        assert_eq!(&decoded.image.as_bytes(0).unwrap()[..4], &[10, 0, 255, 255]);
    }
}
