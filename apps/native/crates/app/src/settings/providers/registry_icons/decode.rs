//! Registry icon pixels, isolated from view/cache state so codec bounds are tested directly.
use gpui::{ImageFormat, RenderImage, SvgRenderer};
use image::{AnimationDecoder, ImageDecoder};
use std::{io::Cursor, sync::Arc};

const EDGE: u32 = 64;
const SOURCE_EDGE: u32 = 2048;
const SOURCE_BYTES: usize = 512 * 1024;

pub(super) fn decode(
    mime: &str,
    bytes: &[u8],
    color: u32,
    svg: &SvgRenderer,
) -> Option<Arc<RenderImage>> {
    if bytes.len() > SOURCE_BYTES {
        return None;
    }
    let format = ImageFormat::from_mime_type(mime)?;
    let mut pixels = if format == ImageFormat::Svg {
        let image = svg_image(bytes, svg)?;
        let size = image.size(0);
        image::RgbaImage::from_raw(
            size.width.0 as u32,
            size.height.0 as u32,
            image.as_bytes(0)?.to_vec(),
        )?
    } else {
        raster(bytes, format)?.thumbnail(EDGE, EDGE).into_rgba8()
    };
    // The web recolors source alpha with currentColor; GPUI takes BGRA pixels.
    for pixel in pixels.pixels_mut() {
        pixel.0[..3].copy_from_slice(&[
            (color >> 8) as u8,
            (color >> 16) as u8,
            (color >> 24) as u8,
        ]);
        pixel.0[3] = (u16::from(pixel.0[3]) * u16::from(color as u8) / 255) as u8;
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(SOURCE_EDGE);
    limits.max_image_height = Some(SOURCE_EDGE);
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits
}
fn raster(bytes: &[u8], format: ImageFormat) -> Option<image::DynamicImage> {
    if format == ImageFormat::Gif {
        let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
        decoder.set_limits(limits()).ok()?;
        return first_frame(decoder.into_frames()).map(image::DynamicImage::ImageRgba8);
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits());
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some(image)
}
fn svg_image(bytes: &[u8], svg: &SvgRenderer) -> Option<Arc<RenderImage>> {
    let mut encoded = String::with_capacity(bytes.len() * 3);
    const HEX: &[u8] = b"0123456789ABCDEF";
    for &byte in bytes {
        encoded.push('%');
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 15) as usize]));
    }
    // The pinned renderer exposes scale only; a fixed outer viewport bounds rasterization.
    let viewport = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{EDGE}" height="{EDGE}"><image width="{EDGE}" height="{EDGE}" href="data:image/svg+xml,{encoded}"/></svg>"#
    );
    let image = svg.render_single_frame(viewport.as_bytes(), 0.5).ok()?;
    image
        .as_bytes(0)?
        .chunks_exact(4)
        .any(|p| p[3] != 0)
        .then_some(image)
}
fn first_frame(
    mut frames: impl Iterator<Item = image::ImageResult<image::Frame>>,
) -> Option<image::RgbaImage> {
    frames.next()?.ok().map(image::Frame::into_buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn renderer() -> SvgRenderer {
        SvgRenderer::new(Arc::new(()))
    }
    fn encoded(format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        let source = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            4,
            image::Rgba([10, 20, 30, 255]),
        ));
        let source = if format == image::ImageFormat::Jpeg {
            image::DynamicImage::ImageRgb8(source.to_rgb8())
        } else {
            source
        };
        source.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }
    #[test]
    fn registry_svg_rasterizes_at_icon_size_and_recolors_alpha() {
        let svg = renderer();
        let source = br#"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="64" viewBox="0 0 128 64"><rect width="128" height="64" fill="red"/></svg>"#;
        let rasterized = svg_image(source, &svg).unwrap();
        assert_eq!(
            (rasterized.size(0).width.0, rasterized.size(0).height.0),
            (64, 64)
        );
        let icon = decode("image/svg+xml", source, 0x12345680, &svg).unwrap();
        assert_eq!(icon.frame_count(), 1);
        assert!(
            icon.as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .any(|p| p == [0x56, 0x34, 0x12, 128])
        );
        assert!(decode("image/svg+xml", b"not an SVG", 0xffffffff, &svg).is_none());
    }
    #[test]
    fn registry_gif_decodes_only_the_first_frame_and_static_formats_still_work() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                        8,
                        4,
                        image::Rgba(color),
                    )))
                    .unwrap();
            }
        }
        let decoded_frames = Cell::new(0);
        let frames = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))
            .unwrap()
            .into_frames();
        let first =
            first_frame(frames.inspect(|_| decoded_frames.set(decoded_frames.get() + 1))).unwrap();
        assert_eq!(decoded_frames.get(), 1);
        assert_eq!(first.get_pixel(0, 0).0, [255, 0, 0, 255]);
        let svg = renderer();
        let gif = decode("image/gif", &bytes, 0x123456ff, &svg).unwrap();
        assert_eq!(gif.frame_count(), 1);
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Bmp, "image/bmp"),
            (image::ImageFormat::Tiff, "image/tiff"),
            (image::ImageFormat::Ico, "image/ico"),
        ] {
            let icon = decode(mime, &encoded(format), 0x123456ff, &svg).unwrap();
            assert_eq!(icon.frame_count(), 1);
            assert_eq!(icon.as_bytes(0).unwrap()[..4], [0x56, 0x34, 0x12, 255]);
        }
        let mut tall = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            SOURCE_EDGE + 1,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(&mut tall, image::ImageFormat::Png)
        .unwrap();
        assert!(decode("image/png", tall.get_ref(), 0xffffffff, &svg).is_none());
        assert!(decode("image/png", &vec![0; 512 * 1024 + 1], 0xffffffff, &svg).is_none());
    }
}
