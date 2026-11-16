//! Pure-Rust image dimension parsing, HEIC metadata bounds validation,
//! compression budgeting, and file/MIME classification parity helpers.
//!
//! Ports `apps/web/src/lib/imageCompression.ts`,
//! `packages/client-runtime/src/imageAttachmentSize.ts`,
//! `packages/shared/src/imageDimensions.ts`,
//! `packages/shared/src/mediaPreview.ts`, and
//! `apps/web/src/components/chat/composerAttachmentFiles.ts`.

use super::{
    Attachment, AttachmentKind, MAX_FILE_BYTES, Upload, extension, file_mime, format_size,
    too_large_message,
};

/// `IMAGE_ATTACHMENT_MAX_EDGE` (`packages/client-runtime/src/imageAttachmentSize.ts`).
pub const IMAGE_ATTACHMENT_MAX_EDGE: u32 = 2000;
/// `MAX_STASH_IMAGE_DATA_URL_CHARS` (`apps/web/src/lib/imageCompression.ts`).
pub const MAX_STASH_IMAGE_DATA_URL_CHARS: usize = 1_300_000;
/// `MAX_COMPRESSIBLE_SOURCE_BYTES` (`apps/web/src/lib/imageCompression.ts`).
pub const MAX_COMPRESSIBLE_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
/// `MAX_HEIC_DECODE_PIXELS` (`apps/web/src/lib/imageCompression.ts`).
pub const MAX_HEIC_DECODE_PIXELS: u64 = 64_000_000;
/// `MAX_HEIC_METADATA_BYTES` (`apps/web/src/lib/imageCompression.ts`).
pub const MAX_HEIC_METADATA_BYTES: usize = 1024 * 1024;
/// `IMAGE_DIMENSIONS_HEADER_BYTES` (`packages/shared/src/imageDimensions.ts`).
pub const IMAGE_DIMENSIONS_HEADER_BYTES: usize = 256 * 1024;

/// `QUALITY_STEPS` (`apps/web/src/lib/imageCompression.ts`).
pub const QUALITY_STEPS: [f32; 4] = [0.92, 0.85, 0.78, 0.68];
/// `FALLBACK_SCALE_STEPS` (`apps/web/src/lib/imageCompression.ts`).
pub const FALLBACK_SCALE_STEPS: [f32; 2] = [0.75, 0.55];

pub const DEFAULT_FALLBACK_UPLOAD_MIME_TYPE: &str = "application/octet-stream";
pub const SVG_ATTACHMENT_MIME_TYPE: &str = "image/svg+xml";
pub const DNG_ATTACHMENT_MIME_TYPE: &str = "image/x-adobe-dng";

/// `WORKSPACE_IMAGE_PREVIEW_EXTENSIONS` (`packages/shared/src/mediaPreview.ts`).
pub const WORKSPACE_IMAGE_PREVIEW_EXTENSIONS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "heic", "heif", "jpeg", "jpg", "png", "svg", "tif", "tiff",
    "webp",
];

/// True when `path` can be opened in the workspace image preview (`isWorkspaceImagePreviewPath`).
pub fn is_workspace_image_preview_path(path: &str) -> bool {
    extension(path).is_some_and(|ext| WORKSPACE_IMAGE_PREVIEW_EXTENSIONS.contains(&ext.as_str()))
}

/// Target MIME type when downscaling an image attachment (`downscalableImageMimeType` in
/// `packages/client-runtime/src/imageAttachmentSize.ts`). Only PNG and JPEG are downscaled; GIF
/// and WebP return `None`.
pub fn downscalable_image_mime_type(mime_type: &str) -> Option<&'static str> {
    let normalized = mime_type.split(';').next().unwrap_or(mime_type).trim().to_ascii_lowercase();
    match normalized.as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        _ => None,
    }
}

/// Proportional resize target within `max_edge` (`fitImageAttachmentSize`).
pub fn fit_image_attachment_size(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_edge == 0 {
        return (0, 0);
    }
    let longest = width.max(height);
    if longest <= max_edge {
        return (width, height);
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    let target_w = ((f64::from(width) * scale).round() as u32).clamp(1, max_edge);
    let target_h = ((f64::from(height) * scale).round() as u32).clamp(1, max_edge);
    (target_w, target_h)
}

/// Proportional resize target within `IMAGE_ATTACHMENT_MAX_EDGE` (`fitImageToMaxEdge`).
pub fn fit_image_to_max_edge(width: u32, height: u32) -> (u32, u32) {
    fit_image_attachment_size(width, height, IMAGE_ATTACHMENT_MAX_EDGE)
}

/// Base64 data URL character length (`dataUrlLength` in `imageCompression.ts`).
pub fn data_url_length(mime_type: &str, byte_length: usize) -> usize {
    let prefix_len = "data:".len() + mime_type.len() + ";base64,".len();
    prefix_len + byte_length.div_ceil(3) * 4
}

/// Encodes `bytes` as a base64 `data:<mime_type>;base64,...` URL.
pub fn data_url(mime_type: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime_type};base64,{encoded}")
}

/// Encoded data-URL character budget matching `(maxBytes / 3) * 4` in `imageCompression.ts`.
pub fn compression_budget_chars(max_bytes: u64) -> usize {
    usize::try_from((max_bytes / 3) * 4).unwrap_or(usize::MAX)
}

/// Output filename matching compressed MIME type (`fileNameForMimeType` in `imageCompression.ts`).
pub fn file_name_for_compressed_mime(name: &str, mime_type: &str) -> String {
    let ext = match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        _ => return name.to_owned(),
    };
    let stem = name.rfind('.').filter(|idx| *idx > 0).map_or(name, |idx| &name[..idx]);
    format!("{stem}.{ext}")
}

/// Target MIME type selection for `compressImageToByteLimit`.
pub fn compression_target_mime(source_mime_type: &str, prefer_jpeg_fallback: bool) -> &'static str {
    if source_mime_type.eq_ignore_ascii_case("image/png") && !prefer_jpeg_fallback {
        "image/png"
    } else {
        "image/jpeg"
    }
}

/// True when a GIF already fits within `max_bytes` and should not be re-encoded into a still frame.
pub fn is_animated_gif_passthrough(mime_type: &str, size_bytes: u64, max_bytes: u64) -> bool {
    mime_type.eq_ignore_ascii_case("image/gif") && size_bytes <= max_bytes
}

/// Checks whether a source file size is within the 50 MiB compressible ceiling.
pub fn is_compressible_source_size(size_bytes: u64) -> bool {
    size_bytes <= MAX_COMPRESSIBLE_SOURCE_BYTES
}

/// True when `prepareImageForAttachment` must decode/resize/re-encode before upload.
///
/// Only HEIC conversions, images over `max_bytes`, and downscalable PNG/JPEG files whose longest
/// edge exceeds `IMAGE_ATTACHMENT_MAX_EDGE` require re-encoding (large WebP/GIF under `max_bytes`
/// stay untouched).
pub fn needs_image_compression(
    mime_type: &str,
    size_bytes: u64,
    max_bytes: u64,
    dimensions: Option<(u32, u32)>,
) -> bool {
    if is_heic_mime(mime_type) {
        return true;
    }
    if size_bytes > max_bytes {
        return true;
    }
    downscalable_image_mime_type(mime_type).is_some()
        && dimensions.is_some_and(|(w, h)| w > IMAGE_ATTACHMENT_MAX_EDGE || h > IMAGE_ATTACHMENT_MAX_EDGE)
}

fn normalized_mime_head(raw_mime_type: Option<&str>) -> Option<String> {
    raw_mime_type.map(|s| {
        s.split(';')
            .next()
            .unwrap_or(s)
            .trim()
            .to_ascii_lowercase()
    })
}

fn has_unknown_mime(raw_mime_type: Option<&str>) -> bool {
    match normalized_mime_head(raw_mime_type).as_deref() {
        None | Some("") | Some("application/octet-stream") => true,
        Some(_) => false,
    }
}

fn is_heic_mime(mime_type: &str) -> bool {
    let lower = mime_type.split(';').next().unwrap_or(mime_type).trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "image/heic" | "image/heif" | "image/heic-sequence" | "image/heif-sequence"
    )
}

/// `isHeicImageFile` (`apps/web/src/lib/imageCompression.ts`): matches explicit HEIC/HEIF MIME
/// types, or `.heic`/`.heif` filename when MIME is empty or `application/octet-stream`.
pub fn is_heic_image_file(name: &str, mime_type: Option<&str>) -> bool {
    if mime_type.is_some_and(is_heic_mime) {
        return true;
    }
    has_unknown_mime(mime_type) && matches!(extension(name).as_deref(), Some("heic" | "heif"))
}

/// Result of inspecting a HEIC/HEIF container's `ispe` primary dimensions before decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeicDimensionValidation {
    Valid,
    TooLarge { width: u32, height: u32 },
    Unreadable,
}

/// Validates HEIC/HEIF ISO BMFF `meta` -> `iprp` -> `ipco` -> `ispe` box dimensions before
/// attempting native/browser decoding (`validateHeicImageDimensions` in `imageCompression.ts`).
pub fn validate_heic_image_dimensions(bytes: &[u8]) -> HeicDimensionValidation {
    let scan_len = bytes.len().min(MAX_HEIC_METADATA_BYTES);
    let data = &bytes[..scan_len];
    let mut best: Option<(u32, u32)> = None;
    let mut found_too_large: Option<(u32, u32)> = None;

    scan_isobmff_boxes(data, 0, data.len(), false, &mut |box_type, payload| {
        if &box_type == b"ispe" && payload.len() >= 12 {
            let width = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
            let height = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
            if width == 0 || height == 0 {
                return;
            }
            if u64::from(width) * u64::from(height) > MAX_HEIC_DECODE_PIXELS {
                found_too_large.get_or_insert((width, height));
            } else {
                best = Some((width, height));
            }
        }
    });

    if let Some((width, height)) = found_too_large {
        return HeicDimensionValidation::TooLarge { width, height };
    }
    if best.is_some() {
        HeicDimensionValidation::Valid
    } else {
        HeicDimensionValidation::Unreadable
    }
}

fn scan_isobmff_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    in_ipco: bool,
    visitor: &mut impl FnMut([u8; 4], &[u8]),
) {
    let mut offset = start;
    while offset + 8 <= end {
        let size32 = u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
        let box_type = [data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]];
        let (header_len, box_end) = if size32 == 0 {
            (8usize, end)
        } else if size32 == 1 {
            if offset + 16 > end {
                break;
            }
            let size64 = u64::from_be_bytes([
                data[offset + 8],
                data[offset + 9],
                data[offset + 10],
                data[offset + 11],
                data[offset + 12],
                data[offset + 13],
                data[offset + 14],
                data[offset + 15],
            ]);
            let Some(size) = usize::try_from(size64).ok() else {
                break;
            };
            if size < 16 {
                break;
            }
            (16usize, offset.saturating_add(size).min(end))
        } else {
            let size = size32 as usize;
            if size < 8 {
                break;
            }
            (8usize, offset.saturating_add(size).min(end))
        };

        if box_end <= offset + header_len && &box_type != b"ispe" {
            offset = box_end.max(offset + 8);
            continue;
        }

        let payload_start = offset + header_len;
        if in_ipco && &box_type == b"ispe" {
            visitor(box_type, &data[payload_start..box_end]);
        } else if &box_type == b"meta" {
            // `meta` is a FullBox (4 bytes version + flags before child boxes).
            if payload_start + 4 <= box_end {
                scan_isobmff_boxes(data, payload_start + 4, box_end, false, visitor);
            }
        } else if &box_type == b"iprp" {
            scan_isobmff_boxes(data, payload_start, box_end, false, visitor);
        } else if &box_type == b"ipco" {
            scan_isobmff_boxes(data, payload_start, box_end, true, visitor);
        }

        if box_end <= offset {
            break;
        }
        offset = box_end;
    }
}

/// Parses raster image `(width, height)` from a file header (`readImageDimensions` in
/// `packages/shared/src/imageDimensions.ts`), applying EXIF orientation swaps (5..=8) for JPEGs.
pub fn read_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let header = &bytes[..bytes.len().min(IMAGE_DIMENSIONS_HEADER_BYTES)];
    read_png_dimensions(header)
        .or_else(|| read_gif_dimensions(header))
        .or_else(|| read_webp_dimensions(header))
        .or_else(|| read_jpeg_dimensions(header))
}

fn read_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIG || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

fn read_gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 || (&bytes[..6] != b"GIF87a" && &bytes[..6] != b"GIF89a") {
        return None;
    }
    let width = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
    let height = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    (width > 0 && height > 0).then_some((width, height))
}

fn read_webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 16 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &bytes[12..16];
    if chunk == b"VP8 " && bytes.len() >= 30 {
        if bytes[23] == 0x9d && bytes[24] == 0x01 && bytes[25] == 0x2a {
            let raw_w = u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3fff;
            let raw_h = u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3fff;
            let w = u32::from(raw_w);
            let h = u32::from(raw_h);
            return (w > 0 && h > 0).then_some((w, h));
        }
    } else if chunk == b"VP8L" && bytes.len() >= 25 && bytes[20] == 0x2f {
        let b0 = u32::from(bytes[21]);
        let b1 = u32::from(bytes[22]);
        let b2 = u32::from(bytes[23]);
        let b3 = u32::from(bytes[24]);
        let bits = b0 | (b1 << 8) | (b2 << 16) | (b3 << 24);
        let w = (bits & 0x3fff) + 1;
        let h = ((bits >> 14) & 0x3fff) + 1;
        return Some((w, h));
    } else if chunk == b"VP8X" && bytes.len() >= 30 {
        let w = (u32::from(bytes[24]) | (u32::from(bytes[25]) << 8) | (u32::from(bytes[26]) << 16)) + 1;
        let h = (u32::from(bytes[27]) | (u32::from(bytes[28]) << 8) | (u32::from(bytes[29]) << 16)) + 1;
        return Some((w, h));
    }
    None
}

fn read_jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut orientation = 1u16;
    let mut pos = 2usize;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos] != 0xff {
            pos += 1;
        }
        while pos < bytes.len() && bytes[pos] == 0xff {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;

        if marker == 0xd8 || marker == 0xd9 || marker == 0x00 || (0xd0..=0xd7).contains(&marker) {
            if marker == 0xd9 {
                break;
            }
            continue;
        }
        if marker == 0xda {
            break;
        }

        if pos + 2 > bytes.len() {
            break;
        }
        let seg_len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
        if seg_len < 2 || pos + seg_len > bytes.len() {
            break;
        }

        if marker == 0xe1 && seg_len >= 8 {
            if let Some(exif_orientation) = parse_jpeg_exif_orientation(&bytes[pos + 2..pos + seg_len]) {
                orientation = exif_orientation;
            }
        } else if is_jpeg_sof_marker(marker) && pos + 9 <= bytes.len() {
            let height = u32::from(u16::from_be_bytes([bytes[pos + 3], bytes[pos + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]));
            if width > 0 && height > 0 {
                return Some(if (5..=8).contains(&orientation) {
                    (height, width)
                } else {
                    (width, height)
                });
            }
        }

        pos += seg_len;
    }
    None
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn parse_jpeg_exif_orientation(app1: &[u8]) -> Option<u16> {
    if app1.len() < 14 || &app1[..6] != b"Exif\0\0" {
        return None;
    }
    let tiff = &app1[6..];
    let little_endian = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |offset: usize| -> Option<u16> {
        let slice = tiff.get(offset..offset + 2)?;
        Some(if little_endian {
            u16::from_le_bytes([slice[0], slice[1]])
        } else {
            u16::from_be_bytes([slice[0], slice[1]])
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let slice = tiff.get(offset..offset + 4)?;
        Some(if little_endian {
            u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
        } else {
            u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]])
        })
    };

    if read_u16(2)? != 42 {
        return None;
    }
    let ifd_offset = usize::try_from(read_u32(4)?).ok()?;
    let entry_count = usize::from(read_u16(ifd_offset)?);
    for i in 0..entry_count {
        let entry = ifd_offset + 2 + i * 12;
        if read_u16(entry)? == 0x0112 {
            return read_u16(entry + 8);
        }
    }
    None
}

/// Normalizes an input MIME type / filename to one of the four provider-supported image MIME types
/// (`normalizeComposerImageFileMimeType` in `composerAttachmentFiles.ts`). Only infers from
/// extension when `raw_mime_type` is empty or `application/octet-stream`.
pub fn normalize_composer_image_mime_type(name: &str, raw_mime_type: Option<&str>) -> Option<&'static str> {
    match normalized_mime_head(raw_mime_type).as_deref() {
        Some("image/gif") => return Some("image/gif"),
        Some("image/jpeg" | "image/jpg") => return Some("image/jpeg"),
        Some("image/png" | "image/x-png") => return Some("image/png"),
        Some("image/webp") => return Some("image/webp"),
        _ => {}
    }

    if !has_unknown_mime(raw_mime_type) {
        return None;
    }

    match extension(name).as_deref() {
        Some("gif") => Some("image/gif"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

/// `isDngFile` (`composerAttachmentFiles.ts`).
pub fn is_dng_attachment(name: &str, raw_mime_type: Option<&str>) -> bool {
    matches!(
        normalized_mime_head(raw_mime_type).as_deref(),
        Some("image/x-adobe-dng" | "image/x-dng" | "image/dng")
    ) || extension(name).as_deref() == Some("dng")
}

/// `isSvgAttachment` (`composerAttachmentFiles.ts`).
pub fn is_svg_attachment(name: &str, raw_mime_type: Option<&str>) -> bool {
    normalized_mime_head(raw_mime_type).as_deref() == Some(SVG_ATTACHMENT_MIME_TYPE)
        || extension(name).as_deref() == Some("svg")
}

/// Resolves the MIME type under which a non-raster file attachment is staged
/// (`composerFileAttachmentMimeType` in `composerAttachmentFiles.ts`).
pub fn composer_file_attachment_mime_type(name: &str, raw_mime_type: Option<&str>) -> String {
    if is_dng_attachment(name, raw_mime_type) {
        return DNG_ATTACHMENT_MIME_TYPE.to_owned();
    }
    if is_svg_attachment(name, raw_mime_type) {
        return SVG_ATTACHMENT_MIME_TYPE.to_owned();
    }
    if let Some(raw) = raw_mime_type
        .map(|s| s.split(';').next().unwrap_or(s).trim())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(DEFAULT_FALLBACK_UPLOAD_MIME_TYPE))
    {
        return raw.to_ascii_lowercase();
    }
    file_mime(extension(name).as_deref()).to_owned()
}

/// Full MIME + filename classification matching `classifyComposerAttachmentFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeClassified {
    Image { mime_type: String, convertible_heic: bool },
    File { mime_type: String },
    UnsupportedImage,
}

/// Classifies an incoming picked/dropped/pasted file using both its filename and any OS/browser MIME
/// hint, matching `classifyComposerAttachmentFile` in `composerAttachmentFiles.ts`.
pub fn classify_file_with_mime(name: &str, raw_mime_type: Option<&str>) -> MimeClassified {
    if is_svg_attachment(name, raw_mime_type) {
        return MimeClassified::File {
            mime_type: SVG_ATTACHMENT_MIME_TYPE.to_owned(),
        };
    }
    if is_dng_attachment(name, raw_mime_type) {
        return MimeClassified::File {
            mime_type: DNG_ATTACHMENT_MIME_TYPE.to_owned(),
        };
    }
    if is_heic_image_file(name, raw_mime_type) {
        return MimeClassified::Image {
            mime_type: normalized_mime_head(raw_mime_type)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "image/heic".to_owned()),
            convertible_heic: true,
        };
    }
    if let Some(mime_type) = normalize_composer_image_mime_type(name, raw_mime_type) {
        return MimeClassified::Image {
            mime_type: mime_type.to_owned(),
            convertible_heic: false,
        };
    }

    let raw_lower = normalized_mime_head(raw_mime_type);
    if !raw_lower
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
    {
        return MimeClassified::File {
            mime_type: composer_file_attachment_mime_type(name, raw_mime_type),
        };
    }

    if is_workspace_image_preview_path(name) {
        MimeClassified::File {
            mime_type: composer_file_attachment_mime_type(name, raw_mime_type),
        }
    } else {
        MimeClassified::UnsupportedImage
    }
}

/// `composerImageDedupKey` (`apps/web/src/components/chat/composerAttachmentFiles.ts`).
pub fn composer_image_dedup_key(mime_type: &str, size_bytes: u64, name: &str) -> String {
    format!("{mime_type}\0{size_bytes}\0{name}")
}

/// `fileAttachmentStagingLimit` (`apps/web/src/components/chat/composerAttachmentFiles.ts`).
pub fn file_attachment_staging_limit(
    capability_known: bool,
    supports_attachment_uploads: bool,
    max_file_attachment_bytes: Option<u64>,
) -> Option<u64> {
    if !capability_known {
        return Some(MAX_FILE_BYTES);
    }
    if !supports_attachment_uploads {
        return None;
    }
    max_file_attachment_bytes.map(|limit| limit.min(MAX_FILE_BYTES))
}

/// `fileAttachmentCapabilityBlockReason` (`composerAttachmentFiles.ts`).
pub fn file_attachment_capability_block_reason(
    capability_known: bool,
    supports_attachment_uploads: bool,
    max_file_attachment_bytes: Option<u64>,
    files: &[(&str, u64)],
) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    if !capability_known {
        return Some("Waiting for the server before file attachments can send".to_owned());
    }
    let Some(limit) = file_attachment_staging_limit(
        capability_known,
        supports_attachment_uploads,
        max_file_attachment_bytes,
    ) else {
        return Some(
            "This server does not accept file attachments right now. Remove the files to send."
                .to_owned(),
        );
    };
    files
        .iter()
        .find(|(_, size_bytes)| *size_bytes > limit)
        .map(|(name, _)| too_large_message(name, limit))
}

/// Returns staged attachment IDs that must be released when file upload capability turns off
/// (`attachmentsToReleaseOnUploadCapabilityLoss` in `composerAttachmentFiles.ts`).
///
/// Releases images and unpersisted files (`is_persisted_file == false`), while keeping hydrated or
/// uploaded file entries that carry a server `uploadedAttachmentId`.
pub fn attachments_to_release_on_upload_capability_loss(
    attachments: &[Attachment],
) -> Vec<String> {
    attachments
        .iter()
        .filter(|attachment| {
            !(attachment.kind == AttachmentKind::File
                && matches!(attachment.upload, Upload::Ready { .. }))
        })
        .map(|attachment| attachment.id.clone())
        .collect()
}

/// `shouldHandleComposerAttachmentPaste` (`composerAttachmentFiles.ts`).
pub fn should_handle_attachment_paste(files: &[(&str, Option<&str>)], plain_text: &str) -> bool {
    if files.iter().any(|(name, mime)| {
        is_dng_attachment(name, *mime)
            || is_svg_attachment(name, *mime)
            || matches!(
                classify_file_with_mime(name, *mime),
                MimeClassified::Image { .. } | MimeClassified::UnsupportedImage
            )
    }) {
        return true;
    }
    if !plain_text.is_empty() {
        return false;
    }
    files
        .iter()
        .any(|(name, mime)| matches!(classify_file_with_mime(name, *mime), MimeClassified::File { .. }))
}

/// Human-readable HEIC validation rejection message (`imageCompression.ts`).
pub fn heic_validation_error_message(name: &str, validation: HeicDimensionValidation) -> Option<String> {
    match validation {
        HeicDimensionValidation::Valid => None,
        HeicDimensionValidation::TooLarge { width, height } => Some(format!(
            "'{name}' is {width}x{height}px, which exceeds the {} MP decode limit.",
            MAX_HEIC_DECODE_PIXELS / 1_000_000
        )),
        HeicDimensionValidation::Unreadable => Some(format!(
            "Could not read HEIC dimensions for '{name}'; file size ceiling is {}.",
            format_size(MAX_COMPRESSIBLE_SOURCE_BYTES)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_budgets_and_respects_downscalable_mime_types() {
        assert_eq!(fit_image_to_max_edge(4000, 1000), (2000, 500));
        assert_eq!(fit_image_to_max_edge(800, 600), (800, 600));
        assert_eq!(fit_image_to_max_edge(10_000, 1), (2000, 1));
        assert_eq!(fit_image_to_max_edge(0, 100), (0, 0));

        assert_eq!(downscalable_image_mime_type("IMAGE/PNG"), Some("image/png"));
        assert_eq!(downscalable_image_mime_type("image/jpg"), Some("image/jpeg"));
        assert_eq!(downscalable_image_mime_type("image/webp"), None);
        assert_eq!(downscalable_image_mime_type("image/gif"), None);

        // Large WebP/GIF already under max_bytes does NOT trigger re-encode.
        assert!(!needs_image_compression("image/webp", 500_000, 1_000_000, Some((4000, 3000))));
        assert!(!needs_image_compression("image/gif", 500_000, 1_000_000, Some((4000, 3000))));
        assert!(needs_image_compression("image/png", 500_000, 1_000_000, Some((4000, 3000))));
        assert!(needs_image_compression("image/heic", 500_000, 1_000_000, Some((100, 100))));

        assert_eq!(compression_budget_chars(300), 400);
        assert_eq!(data_url_length("image/jpeg", 3), "data:image/jpeg;base64,".len() + 4);
        assert_eq!(file_name_for_compressed_mime("photo.heic", "image/jpeg"), "photo.jpg");
        assert_eq!(file_name_for_compressed_mime("README", "image/png"), "README.png");
        assert!(is_animated_gif_passthrough("image/gif", 1024, 2048));
        assert!(!is_animated_gif_passthrough("image/gif", 4096, 2048));
    }

    #[test]
    fn reads_png_gif_webp_and_jpeg_with_exif_orientation() {
        let mut png = vec![0u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[12..16].copy_from_slice(b"IHDR");
        png[16..20].copy_from_slice(&1280u32.to_be_bytes());
        png[20..24].copy_from_slice(&720u32.to_be_bytes());
        assert_eq!(read_image_dimensions(&png), Some((1280, 720)));

        let mut gif = Vec::from(*b"GIF89a");
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        assert_eq!(read_image_dimensions(&gif), Some((640, 480)));

        let mut webp = vec![0u8; 30];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..16].copy_from_slice(b"WEBPVP8X");
        let w = 1919u32.to_le_bytes();
        let h = 1079u32.to_le_bytes();
        webp[24..27].copy_from_slice(&w[..3]);
        webp[27..30].copy_from_slice(&h[..3]);
        assert_eq!(read_image_dimensions(&webp), Some((1920, 1080)));

        let mut jpeg = vec![0xff, 0xd8];
        let tiff: Vec<u8> = vec![
            b'E', b'x', b'i', b'f', 0, 0,
            b'M', b'M', 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08,
            0x00, 0x01,
            0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00,
        ];
        jpeg.extend_from_slice(&[0xff, 0xe1]);
        jpeg.extend_from_slice(&((tiff.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&tiff);
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 0x08]);
        jpeg.extend_from_slice(&600u16.to_be_bytes());
        jpeg.extend_from_slice(&800u16.to_be_bytes());
        jpeg.extend_from_slice(&[0x03, 0x01, 0x22, 0x00]);
        assert_eq!(read_image_dimensions(&jpeg), Some((600, 800)));
    }

    fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        out.extend_from_slice(box_type);
        out.extend_from_slice(payload);
        out
    }

    fn make_heic_with_ispe(width: u32, height: u32) -> Vec<u8> {
        let mut ispe_payload = vec![0u8; 4];
        ispe_payload.extend_from_slice(&width.to_be_bytes());
        ispe_payload.extend_from_slice(&height.to_be_bytes());
        let ispe = make_box(b"ispe", &ispe_payload);
        let ipco = make_box(b"ipco", &ispe);
        let iprp = make_box(b"iprp", &ipco);
        let mut meta_payload = vec![0u8; 4];
        meta_payload.extend_from_slice(&iprp);
        make_box(b"meta", &meta_payload)
    }

    #[test]
    fn validates_heic_ispe_pixel_ceilings() {
        assert_eq!(
            validate_heic_image_dimensions(&make_heic_with_ispe(4032, 3024)),
            HeicDimensionValidation::Valid
        );
        assert_eq!(
            validate_heic_image_dimensions(&make_heic_with_ispe(10_000, 7_000)),
            HeicDimensionValidation::TooLarge {
                width: 10_000,
                height: 7_000
            }
        );
        assert_eq!(
            validate_heic_image_dimensions(&make_heic_with_ispe(0, 100)),
            HeicDimensionValidation::Unreadable
        );
        assert_eq!(
            validate_heic_image_dimensions(b"not-a-heic"),
            HeicDimensionValidation::Unreadable
        );
    }

    #[test]
    fn classifies_mime_dedup_capability_and_paste_matching_upstream() {
        assert_eq!(
            classify_file_with_mime("photo.DNG", Some("image/tiff")),
            MimeClassified::File {
                mime_type: DNG_ATTACHMENT_MIME_TYPE.to_owned(),
            }
        );
        assert_eq!(
            classify_file_with_mime("doc.png", Some("application/pdf")),
            MimeClassified::File {
                mime_type: "application/pdf".to_owned(),
            }
        );
        assert_eq!(
            classify_file_with_mime("photo.heic", Some("text/plain")),
            MimeClassified::File {
                mime_type: "text/plain".to_owned(),
            }
        );
        assert_eq!(
            classify_file_with_mime("live.heic", Some("application/octet-stream")),
            MimeClassified::Image {
                mime_type: "application/octet-stream".to_owned(),
                convertible_heic: true,
            }
        );
        assert_eq!(
            classify_file_with_mime("scan.tiff", None),
            MimeClassified::File {
                mime_type: "application/octet-stream".to_owned(),
            }
        );
        assert_eq!(
            classify_file_with_mime("photo.tiff", Some("image/tiff")),
            MimeClassified::File {
                mime_type: "image/tiff".to_owned(),
            }
        );
        assert_eq!(
            classify_file_with_mime("layers.psd", Some("image/vnd.adobe.photoshop")),
            MimeClassified::UnsupportedImage
        );

        assert_eq!(
            composer_image_dedup_key("image/png", 1234, "shot.png"),
            "image/png\x001234\0shot.png"
        );

        assert_eq!(file_attachment_capability_block_reason(false, false, None, &[]), None);
        assert_eq!(
            file_attachment_capability_block_reason(false, false, None, &[("pending.zip", 10)]),
            Some("Waiting for the server before file attachments can send".to_owned())
        );
        assert_eq!(
            file_attachment_capability_block_reason(true, true, None, &[("pending.zip", 10)]),
            Some(
                "This server does not accept file attachments right now. Remove the files to send."
                    .to_owned()
            )
        );
        assert!(
            file_attachment_capability_block_reason(true, true, Some(5), &[("pending.zip", 10)])
                .is_some()
        );

        assert!(should_handle_attachment_paste(
            &[("photo.dng", Some("application/octet-stream"))],
            "Inspect this"
        ));
        assert!(!should_handle_attachment_paste(
            &[("photo.tiff", Some("image/tiff"))],
            "Image caption"
        ));
        assert!(!should_handle_attachment_paste(
            &[("clipboard.rtf", Some("application/rtf"))],
            "Copied text"
        ));

        let attachments = vec![
            Attachment {
                id: "image-1".into(),
                kind: AttachmentKind::Image,
                name: "photo.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 3,
                pasted_text: false,
                upload: Upload::Ready {
                    attachment_id: "img-ready".into(),
                },
            },
            Attachment {
                id: "file-uploading".into(),
                kind: AttachmentKind::File,
                name: "fresh.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 3,
                pasted_text: false,
                upload: Upload::Uploading,
            },
            Attachment {
                id: "file-hydrated".into(),
                kind: AttachmentKind::File,
                name: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                size_bytes: 3,
                pasted_text: false,
                upload: Upload::Ready {
                    attachment_id: "pending-report-pdf".into(),
                },
            },
        ];
        assert_eq!(
            attachments_to_release_on_upload_capability_loss(&attachments),
            vec!["image-1".to_owned(), "file-uploading".to_owned()]
        );
    }
}
