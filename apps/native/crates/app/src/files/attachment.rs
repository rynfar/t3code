use t3_core::files::binary::BINARY_PREVIEW_MAX_BYTES;
use t3_core::files::media_preview::{MAX_AUDIO_BYTES, inspect_audio_bytes};
use t3_core::files::office::{OFFICE_PREVIEW_MAX_BYTES, OfficeKind, OfficePreview};
use t3_core::files::pdf::PDF_MAX_BYTES;
use t3_core::files::{
    FileContents, ImageKind, binary_container_kind, image_kind, is_audio, is_font, is_pdf,
    office_kind,
};
use t3_core::media::AssetResource;
use t3_core::right_panel::FileAttachment;

use super::preview::{Key, TextFile, build_text_file};
use super::{audio, binary, font, image, office, pdf};

const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttachmentTarget {
    pub environment: String,
    pub file: FileAttachment,
}

impl AttachmentTarget {
    pub fn resource(&self) -> Option<AssetResource> {
        (!self.file.id.trim().is_empty()).then(|| AssetResource::Attachment {
            attachment_id: self.file.id.clone(),
        })
    }
}

pub(super) enum DecodedAttachment {
    Audio(audio::DecodedAudio),
    Binary(binary::DecodedBinary),
    Font(font::DecodedFont),
    Image(image::Decoded),
    Office(OfficePreview),
    Pdf(pdf::DecodedPdf),
    Text(Box<TextFile>),
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Audio,
    Binary,
    Font,
    Image(ImageKind),
    Office(OfficeKind),
    Pdf,
    Text,
}

impl Kind {
    pub fn of(file: &FileAttachment) -> Self {
        let path = &file.name;
        let mime = file
            .mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if is_pdf(path) || mime == "application/pdf" {
            return Self::Pdf;
        }
        if let Some(kind) = office_kind(path) {
            return Self::Office(kind);
        }
        if let Some(kind) = image_kind(path) {
            return Self::Image(kind);
        }
        if mime.starts_with("image/") {
            return Self::Image(if mime == "image/svg+xml" {
                ImageKind::Svg
            } else {
                ImageKind::Raster
            });
        }
        if is_audio(path) || mime.starts_with("audio/") {
            return Self::Audio;
        }
        if is_font(path) || mime.starts_with("font/") {
            return Self::Font;
        }
        if binary_container_kind(path).is_some()
            || matches!(
                mime.as_str(),
                "application/vnd.sqlite3" | "application/x-sqlite3"
            )
        {
            return Self::Binary;
        }
        Self::Text
    }

    pub fn limit(self) -> usize {
        match self {
            Self::Audio => MAX_AUDIO_BYTES,
            Self::Binary => BINARY_PREVIEW_MAX_BYTES,
            Self::Font => 16 * 1024 * 1024,
            Self::Image(_) => 32 * 1024 * 1024,
            Self::Office(_) => OFFICE_PREVIEW_MAX_BYTES,
            Self::Pdf => PDF_MAX_BYTES,
            Self::Text => MAX_TEXT_BYTES,
        }
    }

    pub fn decode(self, key: Key, bytes: Vec<u8>) -> Result<DecodedAttachment, String> {
        match self {
            Self::Audio => {
                let info = inspect_audio_bytes(&key.path, &bytes);
                Ok(DecodedAttachment::Audio(audio::DecodedAudio {
                    name: key.path.clone(),
                    path: key.path,
                    thread_id: String::new(),
                    info,
                    bytes,
                }))
            }
            Self::Binary => {
                let path = if binary_container_kind(&key.path).is_some() {
                    key.path.as_str()
                } else {
                    "attachment.sqlite"
                };
                Ok(DecodedAttachment::Binary(binary::decode(path, bytes)))
            }
            Self::Font => font::decode(bytes).map(DecodedAttachment::Font),
            Self::Image(ImageKind::Unsupported) => Err("No preview for this file".to_owned()),
            Self::Image(kind) => {
                image::decode(bytes, kind == ImageKind::Svg).map(DecodedAttachment::Image)
            }
            Self::Office(kind) => office::decode(kind, bytes).map(DecodedAttachment::Office),
            Self::Pdf => pdf::decode(bytes).map(DecodedAttachment::Pdf),
            Self::Text => {
                let byte_length = bytes.len() as u64;
                let contents = String::from_utf8(bytes)
                    .ok()
                    .filter(|text| !text.contains('\0'))
                    .ok_or_else(|| "No preview for this file".to_owned())?;
                let file = FileContents {
                    relative_path: key.path.clone(),
                    contents,
                    byte_length,
                    truncated: false,
                };
                Ok(DecodedAttachment::Text(Box::new(build_text_file(
                    key, file,
                ))))
            }
        }
    }
}
