//! Composer attachments: how a picked, dropped or pasted file is classified
//! (`web/src/components/chat/composerAttachmentFiles.ts`), the limits of
//! `contracts/src/chatAttachment.ts`, the `attachments.createUploadUrl` request and upload URL
//! (`client-runtime/src/state/attachments.ts`, `web/src/lib/attachmentUploadQueue.ts`), the
//! `ChatAttachment` a turn carries (`getUploadedAttachments`) with its context record
//! (`attachmentContextRecord` in `web/src/lib/composerContextRecords.ts`), large pastes folded
//! into a file (`client-runtime/src/textPaste.ts`), and the attachment-only prompt
//! (`shared/src/composerContextReferences.ts`).

use serde_json::{Value, json};

use super::menu::{ThreadContext, kind_scoped_context_id, referenced_context_ids, sanitize_context_label};

pub const CREATE_UPLOAD_URL: &str = "attachments.createUploadUrl";
pub const DELETE: &str = "attachments.delete";

/// `PROVIDER_SEND_TURN_MAX_ATTACHMENTS`.
pub const MAX_ATTACHMENTS: usize = 100;
/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES`.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
/// `PROVIDER_SEND_TURN_MAX_FILE_BYTES`.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// `PROVIDER_SEND_TURN_MAX_INPUT_CHARS`.
pub const MAX_INPUT_CHARS: usize = 120_000;
/// `PASTED_TEXT_ATTACHMENT_THRESHOLD_BYTES`.
pub const PASTED_TEXT_THRESHOLD_BYTES: usize = 32 * 1024;

/// `ATTACHMENT_ONLY_BOOTSTRAP_PROMPT`: the text a message with only attachments sends.
pub const ATTACHMENT_ONLY_PROMPT: &str =
    "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentKind {
    Image,
    File,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AttachmentKind::Image => "image",
            AttachmentKind::File => "file",
        }
    }
}

/// `ComposerAttachmentFileKind`, with the MIME type the attachment is staged under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    Image { mime_type: &'static str },
    File { mime_type: String },
    UnsupportedImage,
}

fn extension(name: &str) -> Option<String> {
    let dot = name.rfind('.').filter(|dot| *dot > 0)?;
    Some(name[dot + 1..].to_lowercase())
}

/// `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES` by extension (`IMAGE_MIME_TYPE_BY_EXTENSION`).
fn supported_image_mime(extension: &str) -> Option<&'static str> {
    match extension {
        "gif" => Some("image/gif"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The MIME type a file the OS hands over by path is staged under. Paths carry no type, so it
/// comes from the extension, as the browser infers `File.type`.
fn file_mime(extension: Option<&str>) -> &'static str {
    match extension.unwrap_or_default() {
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "svg" => "image/svg+xml",
        "dng" => "image/x-adobe-dng",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// `classifyComposerAttachmentFile` for a file known by name. SVG and DNG stage as files; images
/// providers cannot take (HEIC needs a conversion this client does not do, TIFF, BMP) are refused.
pub fn classify(name: &str) -> Classified {
    let extension = extension(name);
    if let Some(mime_type) = extension.as_deref().and_then(supported_image_mime) {
        return Classified::Image { mime_type };
    }
    match extension.as_deref() {
        Some("heic" | "heif" | "tif" | "tiff" | "bmp" | "avif") => Classified::UnsupportedImage,
        other => Classified::File { mime_type: file_mime(other).to_owned() },
    }
}

/// The image types the clipboard hands over, by GPUI's format name.
pub fn image_mime_for_format(format: &str) -> Option<&'static str> {
    supported_image_mime(&format.to_lowercase())
}

/// `refusal` copy for an image no provider takes.
pub fn unsupported_image_message(name: &str) -> String {
    format!("'{name}' is not a supported image type. Attach GIF, JPEG, PNG, or WebP images.")
}

/// Where an upload stands (`AttachmentUploadState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upload {
    Uploading,
    Ready { attachment_id: String },
    Failed { reason: String },
}

/// One staged attachment: what the strip shows and what a send carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// This client's id for the attachment, which its context record is scoped by.
    pub id: String,
    pub kind: AttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Folded from a large paste (`PastedTextAttachmentSource`).
    pub pasted_text: bool,
    pub upload: Upload,
}

impl Attachment {
    pub fn attachment_id(&self) -> Option<&str> {
        match &self.upload {
            Upload::Ready { attachment_id } => Some(attachment_id),
            _ => None,
        }
    }

    /// `AttachmentCreateUploadUrlInput`: images omit the type.
    pub fn upload_request(&self) -> Value {
        let mut request = json!({ "name": self.name, "mimeType": self.mime_type, "sizeBytes": self.size_bytes });
        if self.kind == AttachmentKind::File {
            request["type"] = json!("file");
        }
        request
    }

    /// The `ChatAttachment` a turn carries, once uploaded.
    pub fn chat_attachment(&self) -> Option<Value> {
        let mut attachment = json!({
            "type": self.kind.as_str(),
            "id": self.attachment_id()?,
            "name": self.name,
            "mimeType": self.mime_type,
            "sizeBytes": self.size_bytes,
        });
        if self.pasted_text {
            attachment["source"] = json!({ "_tag": "pasted-text" });
        }
        Some(attachment)
    }

    pub fn context_id(&self) -> String {
        kind_scoped_context_id(self.kind.as_str(), &self.id)
    }

    /// `attachmentContextRecord`.
    pub fn context_record(&self) -> Option<Value> {
        Some(json!({
            "version": 1,
            "kind": self.kind.as_str(),
            "contextId": self.context_id(),
            "label": sanitize_context_label(&self.name, self.kind.as_str()),
            "attachmentId": self.attachment_id()?,
            "name": self.name,
            "mimeType": self.mime_type,
            "sizeBytes": self.size_bytes,
        }))
    }
}

/// An `attachments.createUploadUrl` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTicket {
    pub attachment_id: String,
    pub relative_url: String,
}

pub fn decode_ticket(reply: &Value) -> Option<UploadTicket> {
    Some(UploadTicket {
        attachment_id: reply.get("attachmentId")?.as_str()?.to_owned(),
        relative_url: reply.get("relativeUrl")?.as_str()?.to_owned(),
    })
}

/// `resolveAssetUrl`: the signed upload path against the server's HTTP origin.
pub fn upload_url(http_base: &str, relative_url: &str) -> Option<String> {
    url::Url::parse(http_base).ok()?.join(relative_url).ok().map(String::from)
}

/// Whether the server runs on this machine, so a local folder path means the same to it.
pub fn is_local_server(http_base: &str) -> bool {
    url::Url::parse(http_base).ok().is_some_and(|url| match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    })
}

pub fn delete_request(attachment_id: &str) -> Value {
    json!({ "attachmentId": attachment_id })
}

/// `fileAttachmentStagingLimit` once the server config is known: its advertised upload size,
/// capped by what a turn may carry, or `None` when it takes no files.
pub fn file_staging_limit(advertised_max_upload_bytes: Option<u64>) -> Option<u64> {
    advertised_max_upload_bytes.map(|bytes| bytes.min(MAX_FILE_BYTES))
}

/// `formatAttachmentSize`.
pub fn format_size(size_bytes: u64) -> String {
    if size_bytes >= 1024 * 1024 {
        format!("{:.1} MB", size_bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", size_bytes.div_ceil(1024).max(1))
    }
}

/// `fileAttachmentTooLargeMessage`.
pub fn too_large_message(name: &str, max_upload_bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    let size = if max_upload_bytes >= MB && max_upload_bytes.is_multiple_of(MB) {
        format!("{} MB", max_upload_bytes / MB)
    } else if max_upload_bytes >= 1024 && max_upload_bytes.is_multiple_of(1024) {
        format!("{} KB", max_upload_bytes / 1024)
    } else {
        format!("{max_upload_bytes} {}", if max_upload_bytes == 1 { "byte" } else { "bytes" })
    };
    format!("'{name}' exceeds the {size} attachment limit.")
}

/// Why a new file cannot join `staged`, as `addComposerAttachments` checks each one.
pub fn admission_error(staged: usize, name: &str, kind: &Classified, size_bytes: u64, file_limit: Option<u64>) -> Option<String> {
    if staged >= MAX_ATTACHMENTS {
        return Some(format!("You can attach up to {MAX_ATTACHMENTS} files per message."));
    }
    match kind {
        Classified::UnsupportedImage => Some(unsupported_image_message(name)),
        Classified::Image { .. } if size_bytes > MAX_IMAGE_BYTES => {
            Some(format!("'{name}' is too large to attach, even after compression."))
        }
        Classified::Image { .. } => None,
        Classified::File { .. } => match file_limit {
            None => Some("This server does not support file attachments.".to_owned()),
            Some(_) if size_bytes == 0 => Some(format!("'{name}' is empty or could not be read.")),
            Some(limit) if size_bytes > limit => Some(too_large_message(name, limit)),
            Some(_) => None,
        },
    }
}

/// `getProviderAttachmentLimitError`.
pub fn limit_error(attachments: &[Attachment]) -> Option<String> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Some(format!("You can attach up to {MAX_ATTACHMENTS} files per message or question response."));
    }
    let image_bytes: u64 = attachments.iter().filter(|a| a.kind == AttachmentKind::Image).map(|a| a.size_bytes).sum();
    (image_bytes > MAX_TOTAL_IMAGE_BYTES).then(|| {
        "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once.".to_owned()
    })
}

/// Why Send waits: an upload still running or one that failed.
pub fn send_block_reason(attachments: &[Attachment]) -> Option<String> {
    if attachments.iter().any(|attachment| attachment.upload == Upload::Uploading) {
        return Some("Wait for attachments to finish uploading".to_owned());
    }
    if let Some(Upload::Failed { reason }) = attachments.iter().map(|a| &a.upload).find(|u| matches!(u, Upload::Failed { .. })) {
        return Some(format!("{reason}. Remove the attachment to send."));
    }
    limit_error(attachments)
}

/// `pastedTextDisposition`: a paste of 32 KiB or more, or one that would overflow the prompt,
/// becomes a file. Measured in bytes and UTF-16 units, as the web measures both.
pub fn folds_into_file(text: &str, bypass: bool, would_exceed_input_limit: bool) -> bool {
    if bypass || text.is_empty() {
        return false;
    }
    would_exceed_input_limit || text.encode_utf16().count() >= PASTED_TEXT_THRESHOLD_BYTES || text.len() >= PASTED_TEXT_THRESHOLD_BYTES
}

/// `wouldTextPasteExceedLimit`, in UTF-16 units like the provider limit.
pub fn paste_exceeds_limit(prompt: &str, selection: std::ops::Range<usize>, pasted: &str) -> bool {
    let units = |text: &str| text.encode_utf16().count();
    let start = selection.start.min(prompt.len());
    let end = selection.end.clamp(start, prompt.len());
    let replaced = prompt.get(start..end).map_or(0, units);
    units(prompt) - replaced + units(pasted) > MAX_INPUT_CHARS
}

/// `nextPastedTextFileName`.
pub fn next_pasted_text_name<'a>(existing: impl IntoIterator<Item = &'a str>) -> String {
    let names: std::collections::HashSet<String> = existing.into_iter().map(str::to_lowercase).collect();
    if !names.contains("pasted-text.txt") {
        return "pasted-text.txt".to_owned();
    }
    (2..).map(|sequence| format!("pasted-text-{sequence}.txt")).find(|name| !names.contains(name)).unwrap_or_default()
}

/// `restoreAttachmentOnlyBootstrapPrompt`: a message with attachments and no words still asks.
pub fn text_for_send(text: &str, has_attachments: bool) -> String {
    if !has_attachments {
        return text.to_owned();
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ATTACHMENT_ONLY_PROMPT.to_owned();
    }
    let lower = trimmed.to_lowercase();
    if lower.strip_prefix("ultrathink:").is_some_and(|rest| rest.trim().is_empty()) {
        return format!("Ultrathink:\n{ATTACHMENT_ONLY_PROMPT}");
    }
    text.to_owned()
}

/// `buildMessageContext`: the thread references the text still links, then every uploaded
/// attachment, or `None` when there is nothing to carry.
pub fn message_context(text: &str, threads: &[ThreadContext], attachments: &[Attachment]) -> Option<Value> {
    let referenced = referenced_context_ids(text);
    let records: Vec<Value> = threads
        .iter()
        .filter(|thread| referenced.contains(&thread.context_id.as_str()))
        .map(ThreadContext::to_record)
        .chain(attachments.iter().filter_map(Attachment::context_record))
        .collect();
    (!records.is_empty()).then(|| json!({ "version": 1, "records": records }))
}

/// The `ChatAttachment`s a send carries; `None` while any is not uploaded.
pub fn chat_attachments(attachments: &[Attachment]) -> Option<Vec<Value>> {
    attachments.iter().map(Attachment::chat_attachment).collect()
}

#[cfg(test)]
mod tests;
