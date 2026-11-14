//! Media in the transcript: image attachments on user messages and images authored in markdown,
//! resolved to what the client fetches. Host files never load directly: they become an
//! `AssetResource` that `assets.createUrl` signs. Ports `client-runtime/src/markdownImages.ts`,
//! `mediaSource.ts`, `mediaReference.ts`, the attachment
//! predicates in `apps/web/src/types.ts` and the media placement of `TimelineUserMessageRow.tsx`.
//! File rows and sent chip sizes port `chat/GuestMessageAttachments.tsx` and
//! `chat/UserMessageContextChips.tsx`. Revision tracking ports `state/assets.ts` and `state/threadDetail.ts` in client-runtime.

mod policy;
pub use policy::*;
mod revision;
pub use revision::{AssetFetch, RevisionedAssetUrl};

use serde_json::{Value, json};

/// `WS_METHODS.assetsCreateUrl`.
pub const CREATE_ASSET_URL: &str = "assets.createUrl";
/// `ASSET_URL_MAX_IN_FLIGHT`: a thread with hundreds of images mints a few URLs at a time.
pub const MAX_IN_FLIGHT: usize = 6;

/// The `AssetResource` variants the transcript asks for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetResource {
    Attachment { attachment_id: String },
    MediaFile { thread_id: String, path: String },
    /// An image a tool returned, by its position among the item's output images.
    ToolOutputImage { thread_id: String, item_id: String, index: usize },
    /// An attachment served inline under its own name and type, as an HTML render frame loads
    /// its page.
    InlineAttachment { attachment_id: String, file_name: String, mime_type: String },
    /// Change-request media hosted behind source-control credentials (`SOURCE_CONTROL_MEDIA_ASSET`).
    SourceControlMedia { cwd: String, url: String, repository: Option<String> },
}

impl AssetResource {
    pub fn to_json(&self) -> Value {
        match self {
            AssetResource::Attachment { attachment_id } => json!({ "_tag": "attachment", "attachmentId": attachment_id }),
            AssetResource::MediaFile { thread_id, path } => json!({ "_tag": "media-file", "threadId": thread_id, "path": path }),
            AssetResource::ToolOutputImage { thread_id, item_id, index } => {
                json!({ "_tag": "tool-output-image", "threadId": thread_id, "itemId": item_id, "index": index })
            }
            AssetResource::InlineAttachment { attachment_id, file_name, mime_type } => json!({
                "_tag": "attachment",
                "attachmentId": attachment_id,
                "fileName": file_name,
                "mimeType": mime_type,
                "disposition": "inline",
            }),
            AssetResource::SourceControlMedia { cwd, url, repository } => {
                let mut map = serde_json::Map::new();
                map.insert("_tag".into(), Value::String("github-media".into()));
                map.insert("cwd".into(), Value::String(cwd.clone()));
                map.insert("url".into(), Value::String(url.clone()));
                if let Some(repository) = repository.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    map.insert("repository".into(), Value::String(repository.to_owned()));
                }
                Value::Object(map)
            }
        }
    }

    /// `AssetCreateUrlInput`.
    pub fn create_url_request(&self) -> Value {
        json!({ "resource": self.to_json() })
    }

    /// Direct host fallback URL when `SourceControlMedia` minting fails (`fallbackSrc={hostMediaUrl}`).
    pub fn fallback_url(&self) -> Option<&str> {
        match self {
            AssetResource::SourceControlMedia { url, .. } => Some(url.as_str()),
            _ => None,
        }
    }
}

/// Where an image's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MediaSource {
    /// Signed through `assets.createUrl`, then fetched from the environment.
    Asset(AssetResource),
    /// Fetched as authored: an `http(s)` URL or a `data:` URL.
    Direct(String),
}

/// `AssetCreateUrlResult`, reduced to what the client loads.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetUrl {
    pub relative_url: String,
    pub dimensions: Option<(u32, u32)>,
    pub expires_at: f64,
    pub file_revision: Option<String>,
}

/// Re-signing a URL does not change decoded pixels when the file revision is known.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetContentKey {
    FileRevision(String),
    Url(String),
}

impl AssetUrl {
    pub fn content_key(&self) -> AssetContentKey {
        self.file_revision.as_ref().map(|revision| AssetContentKey::FileRevision(revision.clone())).unwrap_or_else(|| AssetContentKey::Url(self.relative_url.clone()))
    }
}

pub fn decode_asset_url(value: &Value) -> Result<AssetUrl, String> {
    let relative_url = value.get("relativeUrl").and_then(Value::as_str).filter(|url| !url.trim().is_empty()).ok_or("The server sent an asset without a URL.")?;
    let dimension = |key: &str| value.get("imageDimensions")?.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
    let expires_at = value.get("expiresAt").and_then(Value::as_f64).unwrap_or_default();
    let file_revision = value.get("fileRevision").and_then(Value::as_str).map(str::trim).filter(|revision| !revision.is_empty() && revision.encode_utf16().count() <= 256).map(str::to_owned);
    Ok(AssetUrl { relative_url: relative_url.to_owned(), dimensions: dimension("width").zip(dimension("height")), expires_at, file_revision })
}

/// One image attachment of a user message (`ChatImageAttachment`, or a legacy image file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
}

impl ImageAttachment {
    pub fn source(&self) -> MediaSource {
        MediaSource::Asset(AssetResource::Attachment { attachment_id: self.id.clone() })
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `isImageAttachment`: legacy file attachments can be images; unknown types never are.
pub fn is_image_attachment(attachment: &Value) -> bool {
    match text(attachment, "type") {
        "image" => true,
        "file" => image_mime_type(text(attachment, "mimeType"), text(attachment, "name")).is_some(),
        _ => false,
    }
}

fn image_mime_type(mime_type: &str, name: &str) -> Option<String> {
    let mime = mime_type.trim().to_ascii_lowercase();
    if mime.starts_with("image/") {
        return Some(mime);
    }
    let extension = name.rfind('.').map(|index| &name[index..])?;
    image_mime_from_extension(extension).map(str::to_owned)
}

/// The images a user message shows as media. Preview annotations draw as chips on the web, not
/// in the grid, so they are left out here (`regularImages`).
pub fn image_attachments(attachments: &[Value]) -> Vec<ImageAttachment> {
    attachments
        .iter()
        .filter(|attachment| is_image_attachment(attachment))
        .filter(|attachment| !text(attachment, "name").starts_with("preview-annotation-"))
        .filter_map(|attachment| {
            let id = text(attachment, "id");
            (!id.is_empty()).then(|| ImageAttachment {
                id: id.to_owned(),
                name: text(attachment, "name").to_owned(),
                mime_type: image_mime_type(text(attachment, "mimeType"), text(attachment, "name")).unwrap_or_else(|| "image/*".to_owned()),
            })
        })
        .collect()
}

/// One video attachment of a user message (`isVideoAttachment` in `packages/shared/src/video.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub downloadable: bool,
}

impl VideoAttachment {
    pub fn source(&self) -> MediaSource {
        MediaSource::Asset(AssetResource::Attachment { attachment_id: self.id.clone() })
    }
}

fn is_generic_mime(mime: &str) -> bool {
    matches!(mime, "" | "application/octet-stream" | "binary/octet-stream")
}

/// Resolves video MIME type from explicit metadata or extension (`videoMimeType`).
pub fn video_mime_type(mime_type: &str, name: &str) -> Option<String> {
    let mime = mime_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    if mime.starts_with("video/") && !is_generic_mime(&mime) {
        return Some(mime);
    }
    if !is_generic_mime(&mime) {
        return None;
    }
    let extension = name.rfind('.').map(|index| &name[index..])?;
    video_mime_from_extension(extension).map(str::to_owned)
}

/// Returns true when `attachment` is a user-message video attachment.
pub fn is_video_attachment(attachment: &Value) -> bool {
    let kind = text(attachment, "kind");
    let ty = text(attachment, "type");
    if ty != "file" && ty != "video" && kind != "video" {
        return false;
    }
    video_mime_type(text(attachment, "mimeType"), text(attachment, "name")).is_some()
}

/// Extracts video attachments from a user message (`userVideos`).
pub fn video_attachments(attachments: &[Value]) -> Vec<VideoAttachment> {
    attachments
        .iter()
        .filter(|attachment| is_video_attachment(attachment))
        .filter_map(|attachment| {
            let id = text(attachment, "id");
            (!id.is_empty()).then(|| VideoAttachment {
                id: id.to_owned(),
                name: text(attachment, "name").to_owned(),
                mime_type: video_mime_type(text(attachment, "mimeType"), text(attachment, "name")).unwrap_or_else(|| "video/*".to_owned()),
                downloadable: attachment.get("downloadable").and_then(Value::as_bool) != Some(false),
            })
        })
        .collect()
}

/// Returns true when a workspace file path is a supported video preview (`isWorkspaceVideoPreviewPath`).
pub fn is_workspace_video_preview_path(path: &str) -> bool {
    video_mime_type("", basename(path)).is_some()
}

/// A transcript file row (`GuestMessageAttachments.tsx`), including legacy records without size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttachment {
    pub name: String,
    pub size_bytes: Option<u64>,
}

pub fn other_attachments(attachments: &[Value]) -> Vec<FileAttachment> {
    attachments.iter().filter(|attachment| !is_image_attachment(attachment)).filter_map(|attachment| {
        let name = text(attachment, "name");
        (!name.is_empty()).then(|| FileAttachment { name: name.to_owned(), size_bytes: attachment.get("sizeBytes").and_then(Value::as_u64) })
    }).collect()
}

/// Sent chips use the record's size; files without downloadable bytes say `Sending…`.
pub fn attachment_size_label(record: &Value, attachment: &Value) -> Option<String> {
    let kind = record.get("kind").or_else(|| record.get("type")).and_then(Value::as_str)?;
    if kind == "file" && attachment.get("downloadable").and_then(Value::as_bool) == Some(false) {
        return Some("Sending…".into());
    }
    matches!(kind, "image" | "file").then(|| record.get("sizeBytes").and_then(Value::as_u64).map(crate::composer::attachments::format_size)).flatten()
}

/// Where a user message's images draw relative to its bubble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPlacement {
    /// Sent without words: media alone, no bubble around it (`mediaOnly`).
    Alone,
    /// Sent with words: media above the bubble as its own shape (`mediaAboveText`).
    AboveText,
    /// Guest and agent messages keep a small grid inside the bubble.
    InBubble,
}

pub fn media_placement(images: usize, text_empty: bool, other_files: usize, inline_only: bool) -> Option<MediaPlacement> {
    if images == 0 {
        return None;
    }
    Some(if inline_only {
        MediaPlacement::InBubble
    } else if text_empty && other_files == 0 {
        MediaPlacement::Alone
    } else {
        MediaPlacement::AboveText
    })
}

/// `MarkdownImageSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownImageSource {
    Direct(String),
    WorkspaceFile(String),
    Blocked,
}

fn normalize_destination(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('<') && trimmed.ends_with('>') { &trimmed[1..trimmed.len() - 1] } else { trimmed }
}

fn starts_with_ignore_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len() && value.is_char_boundary(prefix.len()) && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `safeDecodeURIComponent`: the input unchanged when it does not decode to UTF-8.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3).and_then(|pair| std::str::from_utf8(pair).ok()).and_then(|pair| u8::from_str_radix(pair, 16).ok());
            match hex {
                Some(byte) => {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                None => return value.to_owned(),
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

fn split_search_and_hash(value: &str) -> &str {
    let path = value.split('#').next().unwrap_or_default();
    path.split('?').next().unwrap_or_default()
}

pub fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')) || path.starts_with("\\\\")
}

fn strip_slash_prefixed_drive(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.len() >= 4 && bytes[0] == b'/' && is_windows_absolute_path(&path[1..]) { &path[1..] } else { path }
}

/// `markdownDestinationHostPath`.
fn destination_host_path(source: &str) -> Option<String> {
    if starts_with_ignore_case(source, "file:") {
        let url = url::Url::parse(source).ok()?;
        return Some(strip_slash_prefixed_drive(&percent_decode(url.path())).to_owned());
    }
    Some(strip_slash_prefixed_drive(&percent_decode(split_search_and_hash(source))).to_owned())
}

fn is_network_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

/// `workspaceRelativeFilePath`: whether `path` sits under the workspace root.
fn inside_workspace(path: &str, workspace_root: Option<&str>) -> bool {
    let Some(root) = workspace_root.map(|root| root.trim_end_matches(['/', '\\'])).filter(|root| !root.is_empty()) else { return false };
    let normalize = |value: &str| value.replace('\\', "/");
    let (path, root) = (normalize(path), normalize(root));
    let windows = is_windows_absolute_path(&root) || root.starts_with("//");
    let (path, root) = if windows { (path.to_lowercase(), root.to_lowercase()) } else { (path, root) };
    path.strip_prefix(&root).is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// Rendering is not a click, so a network path loads only from the project's own share.
fn host_file_source(path: String, workspace_root: Option<&str>) -> MarkdownImageSource {
    if is_network_path(&path) && !inside_workspace(&path, workspace_root) { MarkdownImageSource::Blocked } else { MarkdownImageSource::WorkspaceFile(path) }
}

fn has_uri_scheme(path: &str) -> bool {
    let mut chars = path.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && path.find(':').is_some_and(|colon| path[1..colon].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
}

fn join_workspace_path(root: &str, relative: &str) -> String {
    let separator = if is_windows_absolute_path(root) { '\\' } else { '/' };
    let root = root.trim_end_matches(['/', '\\']);
    let relative: String = relative.chars().map(|c| if c == '/' || c == '\\' { separator } else { c }).collect();
    format!("{root}{separator}{}", relative.trim_start_matches(separator))
}

/// `classifyMarkdownImageSource`: filesystem paths belong to the environment host and never load
/// without first becoming a signed asset URL.
pub fn classify_markdown_image_source(value: &str, workspace_root: Option<&str>) -> MarkdownImageSource {
    let source = normalize_destination(value);
    if source.is_empty() || source.starts_with('#') || source.starts_with('?') {
        return MarkdownImageSource::Blocked;
    }
    if ["http:", "https:", "data:", "blob:", "//"].iter().any(|prefix| starts_with_ignore_case(source, prefix)) {
        return MarkdownImageSource::Direct(source.to_owned());
    }
    let Some(path) = destination_host_path(source) else { return MarkdownImageSource::Blocked };
    if starts_with_ignore_case(source, "file:") {
        return host_file_source(path, workspace_root);
    }
    if path.is_empty() {
        return MarkdownImageSource::Blocked;
    }
    if path.starts_with('/') || is_windows_absolute_path(&path) {
        return host_file_source(path, workspace_root);
    }
    if has_uri_scheme(&path) || path.starts_with("~/") || path.starts_with("~\\") {
        return MarkdownImageSource::Blocked;
    }
    match workspace_root.filter(|root| !root.is_empty()) {
        Some(root) => MarkdownImageSource::WorkspaceFile(join_workspace_path(root, &path)),
        None => MarkdownImageSource::Blocked,
    }
}

fn image_mime_from_extension(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        ".avif" => "image/avif",
        ".bmp" => "image/bmp",
        ".heic" => "image/heic",
        ".heif" => "image/heif",
        ".gif" => "image/gif",
        ".ico" => "image/x-icon",
        ".jpeg" | ".jpg" => "image/jpeg",
        ".png" => "image/png",
        ".svg" => "image/svg+xml",
        ".tif" | ".tiff" => "image/tiff",
        ".webp" => "image/webp",
        _ => return None,
    })
}

fn video_mime_from_extension(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        ".avi" => "video/x-msvideo",
        ".m4v" | ".mp4" => "video/mp4",
        ".mkv" => "video/x-matroska",
        ".mov" => "video/quicktime",
        ".ogv" => "video/ogg",
        ".webm" => "video/webm",
        _ => return None,
    })
}

const AUDIO_EXTENSIONS: &[&str] = &[".mp3", ".wav", ".ogg", ".oga", ".flac", ".aac", ".m4a", ".opus", ".aiff", ".weba"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// Parsed GitHub/source-control media URL (`gitHubMediaUrl` in `gitHubMedia.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceControlMediaUrl {
    pub fetch_url: String,
    pub file_name: Option<String>,
    pub kind: Option<MediaKind>,
    pub immutable: bool,
}

/// Authored dimension and tag-type metadata preserved across HTML -> Markdown preprocessing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MediaHints {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub explicit_video: bool,
}

impl MediaHints {
    pub fn is_empty(self) -> bool {
        self.width.is_none() && self.height.is_none() && !self.explicit_video
    }
}

const MEDIA_TITLE_MARKER: char = '\u{2063}';
const MEDIA_TITLE_PREFIX: &str = "\u{2063}t3m:";

/// Encodes optional `MediaHints` alongside an optional human title inside an image destination's title.
pub fn encode_media_title(hints: MediaHints, title: Option<&str>) -> Option<String> {
    let user_title = title.map(str::trim).filter(|t| !t.is_empty());
    if hints.is_empty() {
        return user_title.map(str::to_owned);
    }
    let mut parts = Vec::new();
    if let Some(w) = hints.width.filter(|&w| w > 0) {
        parts.push(format!("w={w}"));
    }
    if let Some(h) = hints.height.filter(|&h| h > 0) {
        parts.push(format!("h={h}"));
    }
    if hints.explicit_video {
        parts.push("v=1".into());
    }
    let mut out = format!("{MEDIA_TITLE_PREFIX}{}{MEDIA_TITLE_MARKER}", parts.join(","));
    if let Some(user_title) = user_title {
        out.push_str(user_title);
    }
    Some(out)
}

/// Extracts `MediaHints` and the user-visible title from an encoded Markdown image title.
pub fn decode_media_title(raw: Option<&str>) -> (Option<String>, MediaHints) {
    let Some(raw) = raw else { return (None, MediaHints::default()) };
    let Some(rest) = raw.strip_prefix(MEDIA_TITLE_PREFIX) else {
        let trimmed = raw.trim();
        return ((!trimmed.is_empty()).then(|| trimmed.to_owned()), MediaHints::default());
    };
    let Some((meta, tail)) = rest.split_once(MEDIA_TITLE_MARKER) else {
        return (Some(raw.to_owned()), MediaHints::default());
    };
    let mut hints = MediaHints::default();
    for item in meta.split(',') {
        if let Some(w) = item.strip_prefix("w=").and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0) {
            hints.width = Some(w);
        } else if let Some(h) = item.strip_prefix("h=").and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0) {
            hints.height = Some(h);
        } else if item == "v=1" {
            hints.explicit_video = true;
        }
    }
    let cleaned = tail.trim();
    ((!cleaned.is_empty()).then(|| cleaned.to_owned()), hints)
}

/// Strips any internal `MediaHints` prefix from a string.
pub fn strip_media_title_meta(raw: &str) -> String {
    if !raw.contains(MEDIA_TITLE_PREFIX) {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut cursor = raw;
    while let Some(pos) = cursor.find(MEDIA_TITLE_PREFIX) {
        out.push_str(&cursor[..pos]);
        let after_prefix = &cursor[pos + MEDIA_TITLE_PREFIX.len()..];
        if let Some((_, tail)) = after_prefix.split_once(MEDIA_TITLE_MARKER) {
            cursor = tail;
        } else {
            out.push_str(&cursor[pos..]);
            cursor = "";
            break;
        }
    }
    out.push_str(cursor);
    out
}

fn is_commit_sha(reference: &str) -> bool {
    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

fn file_name_from_segment(segment: &str) -> Option<String> {
    let decoded = percent_decode(segment);
    (!decoded.is_empty()).then_some(decoded)
}

pub fn parse_source_control_media_url(raw_url: &str) -> Option<SourceControlMediaUrl> {
    let parsed = url::Url::parse(raw_url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let pathname = parsed.path();
    let trimmed = pathname.strip_prefix('/')?;
    let segments: Vec<&str> = trimmed.split('/').collect();

    if host == "github.com" || host == "www.github.com" {
        if segments.len() == 3 && segments[0] == "user-attachments" && segments[1] == "assets" && !segments[2].is_empty() {
            return Some(SourceControlMediaUrl {
                fetch_url: format!("https://github.com{pathname}"),
                file_name: file_name_from_segment(segments[2]),
                kind: Some(MediaKind::Video),
                immutable: true,
            });
        }
        if segments.len() == 5
            && !segments[0].is_empty()
            && !segments[1].is_empty()
            && segments[2] == "assets"
            && !segments[3].is_empty()
            && segments[3].bytes().all(|b| b.is_ascii_digit())
            && !segments[4].is_empty()
        {
            return Some(SourceControlMediaUrl {
                fetch_url: format!("https://github.com{pathname}"),
                file_name: file_name_from_segment(segments[4]),
                kind: None,
                immutable: true,
            });
        }
        if segments.len() >= 5
            && !segments[0].is_empty()
            && !segments[1].is_empty()
            && matches!(segments[2], "blob" | "raw")
            && !segments[3].is_empty()
        {
            let rest = segments[4..].join("/");
            if !rest.is_empty() && !segments.last().is_some_and(|s| s.is_empty()) {
                return Some(SourceControlMediaUrl {
                    fetch_url: format!("https://raw.githubusercontent.com/{}/{}/{}/{}", segments[0], segments[1], segments[3], rest),
                    file_name: segments.last().and_then(|s| file_name_from_segment(s)),
                    kind: None,
                    immutable: is_commit_sha(segments[3]),
                });
            }
        }
        return None;
    }

    if host == "raw.githubusercontent.com" {
        if segments.len() >= 4
            && !segments[0].is_empty()
            && !segments[1].is_empty()
            && !segments[2].is_empty()
            && !segments[3..].join("/").is_empty()
            && !segments.last().is_some_and(|s| s.is_empty())
        {
            return Some(SourceControlMediaUrl {
                fetch_url: format!("https://raw.githubusercontent.com{pathname}"),
                file_name: segments.last().and_then(|s| file_name_from_segment(s)),
                kind: None,
                immutable: is_commit_sha(segments[2]),
            });
        }
        return None;
    }

    if host == "media.githubusercontent.com" {
        if segments.len() >= 5
            && segments[0] == "media"
            && !segments[1].is_empty()
            && !segments[2].is_empty()
            && !segments[3].is_empty()
            && !segments[4..].join("/").is_empty()
            && !segments.last().is_some_and(|s| s.is_empty())
        {
            return Some(SourceControlMediaUrl {
                fetch_url: format!("https://media.githubusercontent.com{pathname}"),
                file_name: segments.last().and_then(|s| file_name_from_segment(s)),
                kind: None,
                immutable: false,
            });
        }
        return None;
    }

    None
}

/// Optional source-control credential proxy context for change-request markdown bodies.
#[derive(Debug, Clone, Copy, Default)]
pub struct MediaResolveOptions<'a> {
    pub explicit_video: bool,
    pub source_control_media: bool,
    pub source_control_cwd: Option<&'a str>,
    pub source_control_repository: Option<&'a str>,
}

/// What an authored media destination resolves to (`ResolvedMediaSource`). `source` is `None`
/// when a host file has no thread to read it through (`access: "unavailable"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMedia {
    pub kind: MediaKind,
    pub name: String,
    pub source: Option<MediaSource>,
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or_default()
}

/// `resolveMediaSource`. `image_embed` is markdown image syntax, which may target an endpoint
/// with no recognizable extension.
pub fn resolve_media_source(source: &str, thread_id: Option<&str>, workspace_root: Option<&str>, image_embed: bool) -> Option<ResolvedMedia> {
    resolve_media_source_with_options(source, thread_id, workspace_root, image_embed, MediaResolveOptions::default())
}

pub fn resolve_media_source_with_options(
    source: &str,
    thread_id: Option<&str>,
    workspace_root: Option<&str>,
    image_embed: bool,
    options: MediaResolveOptions<'_>,
) -> Option<ResolvedMedia> {
    let classified = classify_markdown_image_source(source, workspace_root);
    let (path, direct) = match &classified {
        MarkdownImageSource::Blocked => return None,
        MarkdownImageSource::Direct(uri) => {
            let path = if starts_with_ignore_case(uri, "data:") { String::new() } else { percent_decode(split_search_and_hash(uri)) };
            (path, Some(uri.as_str()))
        }
        MarkdownImageSource::WorkspaceFile(path) => (path.clone(), None),
    };
    let normalized_direct = direct.map(|uri| if uri.starts_with("//") { format!("https:{uri}") } else { uri.to_owned() });
    let sc_media = if options.source_control_media {
        normalized_direct.as_deref().and_then(parse_source_control_media_url)
    } else {
        None
    };

    let name = basename(&path);
    let extension = name.rfind('.').map(|index| &name[index..]).unwrap_or_default();
    let data_mime = direct.filter(|uri| starts_with_ignore_case(uri, "data:")).and_then(|uri| uri[5..].split([';', ',']).next()).map(str::to_ascii_lowercase);
    let kind = match data_mime.as_deref() {
        Some(mime) if mime.starts_with("image/") => Some(MediaKind::Image),
        Some(mime) if mime.starts_with("video/") => Some(MediaKind::Video),
        Some(_) => None,
        None if options.explicit_video => Some(MediaKind::Video),
        None if image_mime_from_extension(extension).is_some() => Some(MediaKind::Image),
        None if video_mime_from_extension(extension).is_some() => Some(MediaKind::Video),
        None if AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()) => return None,
        None if sc_media.as_ref().and_then(|sc| sc.kind).is_some() => sc_media.as_ref().and_then(|sc| sc.kind),
        None => image_embed.then_some(MediaKind::Image),
    }?;
    let name = if let Some(sc_name) = sc_media.as_ref().and_then(|sc| sc.file_name.as_deref()).filter(|s| !s.is_empty()) {
        sc_name.to_owned()
    } else if name.is_empty() {
        if kind == MediaKind::Video { "video" } else { "image" }.to_owned()
    } else {
        name.to_owned()
    };
    let source = match (normalized_direct, sc_media, thread_id) {
        (Some(_), Some(sc), _) if options.source_control_cwd.is_some_and(|c| !c.trim().is_empty()) => {
            let cwd = options.source_control_cwd.unwrap().trim().to_owned();
            let repository = options.source_control_repository.map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned);
            Some(MediaSource::Asset(AssetResource::SourceControlMedia { cwd, url: sc.fetch_url, repository }))
        }
        (Some(uri), Some(sc), _) => Some(MediaSource::Direct(if sc.fetch_url.is_empty() { uri } else { sc.fetch_url })),
        (Some(uri), None, _) => Some(MediaSource::Direct(uri)),
        (None, _, Some(thread_id)) => Some(MediaSource::Asset(AssetResource::MediaFile { thread_id: thread_id.to_owned(), path })),
        (None, _, None) => None,
    };
    Some(ResolvedMedia { kind, name, source })
}

/// Decodes a `data:` URL's payload: base64 when it says so, else percent-encoded text.
pub fn decode_data_url(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.get(5..).filter(|_| starts_with_ignore_case(uri, "data:"))?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.to_ascii_lowercase().ends_with(";base64") {
        return Some(percent_decode(payload).into_bytes());
    }
    let mut out = Vec::with_capacity(payload.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in payload.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// The size an image of `width` × `height` decodes to so neither edge exceeds `max_edge`.
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max_edge || longest == 0 {
        return (width, height);
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    let scaled = |edge: u32| ((f64::from(edge) * scale).round() as u32).clamp(1, max_edge);
    (scaled(width), scaled(height))
}

/// `threadDetail.ts::workspaceMutationIdAtom`: checkpoints are in projection order.
pub fn workspace_mutation_id(projection: &crate::thread::ThreadProjection) -> Option<String> {
    let checkpoint = projection.checkpoints.iter().rev().find(|checkpoint| {
        checkpoint.value.get("appRunOrdinal").is_some_and(Value::is_number) && checkpoint.value.get("runId").is_some_and(Value::is_string)
    }).and_then(|checkpoint| checkpoint.value.get("capturedAt")).and_then(Value::as_str);
    crate::diff::workspace_mutation_id(projection.visible_turn_items.iter().map(|row| row.item.as_ref()), checkpoint)
}

#[cfg(test)]
mod tests;
