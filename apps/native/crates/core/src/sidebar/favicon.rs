//! Project favicons: the key helpers in `packages/shared/src/projectFavicon.ts`, the `assets.createUrl`
//! wire shape from `packages/contracts/src/assets.ts`, and the in-memory half of
//! `createProjectFaviconCache` in `packages/client-runtime/src/projectFaviconCache.ts`. Native keeps
//! raw image bytes in memory only, so storage, hydration, data-URL limits and the
//! clear-during-download generations have no counterpart here.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

pub const PROJECT_FAVICON_FALLBACK_MARKER: &str = "project-favicon-missing";
pub const PROJECT_FAVICON_MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_BYTES: usize = 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_ENTRIES: usize = 128;

pub const ASSETS_CREATE_URL: &str = "assets.createUrl";

pub fn project_favicon_url_request(cwd: &str) -> Value {
    json!({ "resource": { "_tag": "project-favicon", "cwd": cwd } })
}

/// `AssetCreateUrlResult`, without the fields favicons ignore.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCreateUrlResult {
    pub relative_url: String,
    pub expires_at: f64,
    #[serde(default)]
    pub file_revision: Option<String>,
}

/// `getProjectFaviconResourceKey`.
pub fn project_favicon_resource_key(environment_id: &str, workspace_root: &str, favicon_path: Option<&str>) -> String {
    json!([environment_id, workspace_root, favicon_path.filter(|path| !path.is_empty())]).to_string()
}

/// The last path segment of `url`, resolved as `new URL(url, "https://t3.invalid")` resolves it.
fn last_path_segment(url: &str) -> Option<String> {
    let resolved = url::Url::parse("https://t3.invalid").ok()?.join(url).ok()?;
    let path = resolved.path();
    Some(path[path.rfind('/').map_or(0, |slash| slash + 1)..].to_owned())
}

/// The revision `getProjectFaviconCacheKey` folds into its key: the signed URL is re-minted on
/// every request, but its file name carries the icon's content hash. Entries are already keyed by
/// environment and workspace, so the revision alone is compared.
pub fn project_favicon_url_revision(url: &str) -> String {
    last_path_segment(url).unwrap_or_else(|| url.to_owned())
}

/// `isProjectFaviconFallbackUrl`: the server found no icon for the project.
pub fn is_project_favicon_fallback_url(url: &str) -> bool {
    last_path_segment(url).is_some_and(|segment| segment == PROJECT_FAVICON_FALLBACK_MARKER)
}

/// Image formats GPUI decodes (`gpui::ImageFormat`). AVIF, HEIC and ICNS are not among them; the
/// server already serves an `.icns` icon's embedded PNG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaviconFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
    Ico,
    Bmp,
    Tiff,
}

/// Binary signatures win over the declared type because GPUI decodes by the format it is handed
/// and a PNG saved as `favicon.ico` is common. SVG has no signature, so it comes from the
/// content type, then the URL's extension, then a look at the text.
pub fn sniff_format(content_type: Option<&str>, url: &str, bytes: &[u8]) -> Option<FaviconFormat> {
    format_from_signature(bytes)
        .or_else(|| content_type.and_then(format_from_content_type))
        .or_else(|| format_from_extension(url))
        .or_else(|| looks_like_svg(bytes).then_some(FaviconFormat::Svg))
}

fn format_from_content_type(content_type: &str) -> Option<FaviconFormat> {
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "image/png" | "image/apng" => FaviconFormat::Png,
        "image/jpeg" | "image/jpg" | "image/pjpeg" => FaviconFormat::Jpeg,
        "image/gif" => FaviconFormat::Gif,
        "image/webp" => FaviconFormat::Webp,
        "image/svg+xml" => FaviconFormat::Svg,
        "image/x-icon" | "image/vnd.microsoft.icon" | "image/ico" | "image/icon" => FaviconFormat::Ico,
        "image/bmp" | "image/x-bmp" | "image/x-ms-bmp" => FaviconFormat::Bmp,
        "image/tiff" => FaviconFormat::Tiff,
        _ => return None,
    })
}

fn format_from_extension(url: &str) -> Option<FaviconFormat> {
    let name = last_path_segment(url)?;
    let (_, extension) = name.rsplit_once('.')?;
    Some(match extension.to_ascii_lowercase().as_str() {
        "png" => FaviconFormat::Png,
        "jpg" | "jpeg" => FaviconFormat::Jpeg,
        "gif" => FaviconFormat::Gif,
        "webp" => FaviconFormat::Webp,
        "svg" => FaviconFormat::Svg,
        "ico" => FaviconFormat::Ico,
        "bmp" => FaviconFormat::Bmp,
        "tif" | "tiff" => FaviconFormat::Tiff,
        _ => return None,
    })
}

fn format_from_signature(bytes: &[u8]) -> Option<FaviconFormat> {
    Some(match bytes {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => FaviconFormat::Png,
        [0xff, 0xd8, 0xff, ..] => FaviconFormat::Jpeg,
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => FaviconFormat::Gif,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => FaviconFormat::Webp,
        [0x00, 0x00, 0x01, 0x00, ..] => FaviconFormat::Ico,
        [b'I', b'I', 0x2a, 0x00, ..] | [b'M', b'M', 0x00, 0x2a, ..] => FaviconFormat::Tiff,
        [b'B', b'M', ..] => FaviconFormat::Bmp,
        _ => return None,
    })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head).trim_ascii_start();
    text.starts_with(b"<") && head.windows(4).any(|window| window == b"<svg")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaviconEntry {
    pub revision: String,
    pub format: FaviconFormat,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup<'a> {
    Hit(&'a FaviconEntry),
    /// Cached at another revision. The web keeps showing it until the new one loads.
    Stale(&'a FaviconEntry),
    Miss,
}

/// A least-recently-used cache of project icons keyed by `project_favicon_resource_key`, bounded
/// by entry count and stored bytes.
#[derive(Debug, Default)]
pub struct FaviconCache {
    entries: HashMap<String, (u64, FaviconEntry)>,
    bytes: usize,
    clock: u64,
}

impl FaviconCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Stored image bytes across every entry.
    pub fn total_bytes(&self) -> usize {
        self.bytes
    }

    /// The cached icon at whatever revision, without touching recency.
    pub fn peek(&self, key: &str) -> Option<&FaviconEntry> {
        self.entries.get(key).map(|(_, entry)| entry)
    }

    /// Marks the entry used and says whether it is current for `revision`.
    pub fn lookup(&mut self, key: &str, revision: &str) -> Lookup<'_> {
        self.clock += 1;
        let Some((used, entry)) = self.entries.get_mut(key) else {
            return Lookup::Miss;
        };
        *used = self.clock;
        if entry.revision == revision { Lookup::Hit(entry) } else { Lookup::Stale(entry) }
    }

    /// Stores an icon as the most recent entry, then drops the oldest until the bounds hold. An
    /// icon larger than the whole byte budget is not stored and leaves the cache untouched.
    pub fn insert(&mut self, key: String, revision: String, bytes: Vec<u8>, format: FaviconFormat) -> bool {
        if bytes.len() > PROJECT_FAVICON_CACHE_MAX_BYTES {
            return false;
        }
        self.clock += 1;
        self.bytes += bytes.len();
        if let Some((_, previous)) = self.entries.insert(key, (self.clock, FaviconEntry { revision, format, bytes })) {
            self.bytes -= previous.bytes.len();
        }
        self.trim();
        true
    }

    pub fn remove(&mut self, key: &str) -> Option<FaviconEntry> {
        let (_, entry) = self.entries.remove(key)?;
        self.bytes -= entry.bytes.len();
        Some(entry)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn trim(&mut self) {
        while self.entries.len() > PROJECT_FAVICON_CACHE_MAX_ENTRIES || self.bytes > PROJECT_FAVICON_CACHE_MAX_BYTES {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, (used, _))| *used).map(|(key, _)| key.clone()) else {
                break;
            };
            self.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://remote.test/api/assets/token-a/vabc-icon.svg";
    const ICON: &[u8] = b"icon";

    fn key(cwd: &str) -> String {
        project_favicon_resource_key("remote", cwd, None)
    }

    fn insert(cache: &mut FaviconCache, cwd: &str, bytes: &[u8]) -> bool {
        cache.insert(key(cwd), project_favicon_url_revision(URL), bytes.to_vec(), FaviconFormat::Png)
    }

    #[test]
    fn keys_isolate_environments_workspaces_and_icon_selections() {
        let mut cache = FaviconCache::default();
        insert(&mut cache, "/workspace", ICON);
        assert_eq!(project_favicon_resource_key("remote", "/workspace", None), r#"["remote","/workspace",null]"#);
        assert_eq!(project_favicon_resource_key("remote", "/workspace", Some("")), key("/workspace"));
        assert_eq!(cache.peek(&key("/workspace")).map(|entry| entry.bytes.as_slice()), Some(ICON));
        assert!(cache.peek(&project_favicon_resource_key("remote", "/workspace", Some("brand.svg"))).is_none());
        assert!(cache.peek(&key("/other")).is_none());
        assert!(cache.peek(&project_favicon_resource_key("other", "/workspace", None)).is_none());
    }

    #[test]
    fn the_revision_is_the_url_file_name_and_survives_re_signing() {
        assert_eq!(project_favicon_url_revision(URL), "vabc-icon.svg");
        assert_eq!(project_favicon_url_revision("/api/assets/token-b/vabc-icon.svg?x=1#y"), "vabc-icon.svg");
        let mut cache = FaviconCache::default();
        insert(&mut cache, "/workspace", ICON);
        let revision = project_favicon_url_revision("/api/assets/token-b/vabc-icon.svg");
        assert!(matches!(cache.lookup(&key("/workspace"), &revision), Lookup::Hit(entry) if entry.bytes == ICON));
        let changed = project_favicon_url_revision("/api/assets/token-c/vdef-icon.svg");
        assert!(matches!(cache.lookup(&key("/workspace"), &changed), Lookup::Stale(entry) if entry.bytes == ICON));
        assert_eq!(cache.lookup(&key("/new"), &changed), Lookup::Miss);
        cache.insert(key("/workspace"), changed.clone(), b"new".to_vec(), FaviconFormat::Png);
        assert!(matches!(cache.lookup(&key("/workspace"), &changed), Lookup::Hit(entry) if entry.bytes == b"new"));
        assert_eq!((cache.len(), cache.total_bytes()), (1, 3));
    }

    #[test]
    fn a_fallback_url_marks_a_missing_icon() {
        assert!(is_project_favicon_fallback_url("/api/assets/token/project-favicon-missing"));
        assert!(is_project_favicon_fallback_url("https://remote.test/api/assets/token/project-favicon-missing?v=1"));
        assert!(!is_project_favicon_fallback_url(URL));
        assert!(!is_project_favicon_fallback_url(""));
        let mut cache = FaviconCache::default();
        insert(&mut cache, "/workspace", ICON);
        assert!(cache.remove(&key("/workspace")).is_some());
        assert!(cache.peek(&key("/workspace")).is_none());
        assert_eq!(cache.total_bytes(), 0);
    }

    #[test]
    fn bounds_individual_images_total_bytes_and_entry_count() {
        let mut cache = FaviconCache::default();
        insert(&mut cache, "/kept", ICON);
        assert!(!insert(&mut cache, "/huge", &vec![0; PROJECT_FAVICON_CACHE_MAX_BYTES + 1]));
        assert!(cache.peek(&key("/huge")).is_none());
        assert!(cache.peek(&key("/kept")).is_some());

        let large = vec![0; 32 * 1024 - 32];
        for i in 0..40 {
            insert(&mut cache, &format!("/large-{i}"), &large);
        }
        assert!(cache.total_bytes() <= PROJECT_FAVICON_CACHE_MAX_BYTES);
        assert!(cache.peek(&key("/large-0")).is_none());
        assert!(cache.peek(&key("/large-39")).is_some());

        for i in 0..=PROJECT_FAVICON_CACHE_MAX_ENTRIES {
            insert(&mut cache, &format!("/small-{i}"), ICON);
        }
        assert_eq!(cache.len(), PROJECT_FAVICON_CACHE_MAX_ENTRIES);
        assert!(cache.peek(&key("/small-0")).is_none());
        assert_eq!(cache.total_bytes(), PROJECT_FAVICON_CACHE_MAX_ENTRIES * ICON.len());
    }

    #[test]
    fn lookups_refresh_recency_and_peeks_do_not() {
        let mut cache = FaviconCache::default();
        for i in 0..PROJECT_FAVICON_CACHE_MAX_ENTRIES {
            insert(&mut cache, &format!("/{i}"), ICON);
        }
        cache.lookup(&key("/0"), "any");
        cache.peek(&key("/1"));
        insert(&mut cache, "/next", ICON);
        insert(&mut cache, "/after", ICON);
        assert!(cache.peek(&key("/0")).is_some());
        assert!(cache.peek(&key("/1")).is_none());
        assert!(cache.peek(&key("/2")).is_none());
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.total_bytes(), 0);
    }

    #[test]
    fn sniffs_the_format_gpui_should_decode() {
        let png = b"\x89PNG\r\n\x1a\n\0\0";
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16"/></svg>"#;
        type Case<'a> = (Option<&'a str>, &'a str, &'a [u8], Option<FaviconFormat>);
        let cases: &[Case] = &[
            (Some("image/png"), "/a/v1-icon.png", png, Some(FaviconFormat::Png)),
            (Some("image/x-icon"), "/a/v1-favicon.ico", png, Some(FaviconFormat::Png)),
            (Some("image/x-icon"), "/a/v1-favicon.ico", b"\0\0\x01\0\x01\0", Some(FaviconFormat::Ico)),
            (Some("image/jpeg"), "/a/v1-icon", b"\xff\xd8\xff\xe0", Some(FaviconFormat::Jpeg)),
            (None, "/a/v1-icon", b"GIF89a....", Some(FaviconFormat::Gif)),
            (None, "/a/v1-icon", b"RIFF\x10\0\0\0WEBPVP8 ", Some(FaviconFormat::Webp)),
            (None, "/a/v1-icon", b"BM\x36\0", Some(FaviconFormat::Bmp)),
            (None, "/a/v1-icon", b"II*\0", Some(FaviconFormat::Tiff)),
            (Some("image/svg+xml; charset=utf-8"), "/a/v1-icon", svg, Some(FaviconFormat::Svg)),
            (Some("text/plain"), "/a/v1-icon.svg", svg, Some(FaviconFormat::Svg)),
            (None, "/a/v1-icon", b"\xef\xbb\xbf\n<?xml version=\"1.0\"?><svg/>", Some(FaviconFormat::Svg)),
            (Some("image/webp"), "/a/v1-icon.webp", b"?", Some(FaviconFormat::Webp)),
            (Some("IMAGE/GIF"), "/a/v1-icon", b"?", Some(FaviconFormat::Gif)),
            (None, "/a/v1-ICON.JPG", b"?", Some(FaviconFormat::Jpeg)),
            (Some("image/avif"), "/a/v1-icon.avif", b"\0\0\0\x1cftypavif", None),
            (Some("image/icns"), "/a/v1-icon.icns", b"icns\0\0", None),
            (Some("text/html"), "/a/v1-icon", b"<html>missing</html>", None),
            (None, "/a/v1-icon", b"", None),
        ];
        for (content_type, url, bytes, expected) in cases {
            assert_eq!(sniff_format(*content_type, url, bytes), *expected, "{content_type:?} {url}");
        }
    }

    #[test]
    fn decodes_the_create_url_result_and_builds_the_request() {
        assert_eq!(project_favicon_url_request("/workspace"), json!({ "resource": { "_tag": "project-favicon", "cwd": "/workspace" } }));
        let result: AssetCreateUrlResult = serde_json::from_value(json!({
            "relativeUrl": "/api/assets/t/vabc-icon.svg",
            "expiresAt": 1_700_000_000_000_i64,
            "sourcePath": "icon.svg",
            "future": true,
        }))
        .unwrap();
        assert_eq!(result.relative_url, "/api/assets/t/vabc-icon.svg");
        assert_eq!(result.file_revision, None);
        let revised: AssetCreateUrlResult = serde_json::from_value(json!({ "relativeUrl": "/x", "expiresAt": 1.5, "fileRevision": "r1" })).unwrap();
        assert_eq!(revised.file_revision.as_deref(), Some("r1"));
    }
}
