//! Ports `state/assets.ts` and `files/FileMarkdownPreview.tsx`.

use super::{AssetResource, image_mime_from_extension, video_mime_from_extension, is_windows_absolute_path};

pub const ASSET_URL_STALE_TIME_MS: f64 = 5. * 60_000.;
pub const ASSET_URL_REFRESH_INTERVAL_MS: f64 = 30. * 60_000.;

pub fn asset_is_stale(settled_at: Option<f64>, now: f64) -> bool {
    settled_at.is_some_and(|at| now - at >= ASSET_URL_STALE_TIME_MS)
}

pub fn asset_refresh_delay(settled_at: Option<f64>, now: f64, visible: bool) -> Option<f64> {
    visible.then(|| (ASSET_URL_REFRESH_INTERVAL_MS - settled_at.map_or(0., |at| now - at)).max(0.))
}

pub fn asset_refresh_schedule(current: Option<f64>, now: f64, delay: Option<f64>) -> Option<(f64, f64)> {
    let delay = delay?;
    let deadline = now + delay;
    current.is_none_or(|current| deadline < current - 1000.).then_some((deadline, delay))
}

pub fn markdown_image_base_dir(cwd: &str, path: &str) -> String {
    let Some(index) = path.rfind(['/', '\\']) else { return cwd.to_owned() };
    let end = if index == 2 && is_windows_absolute_path(path) { index + 1 } else { index.max(1) };
    crate::files::absolute_path(&path[..end], cwd)
}

pub fn workspace_mutation_changed(root_changed: bool, previous: Option<&str>, next: Option<&str>) -> bool {
    !root_changed && previous != next
}

pub fn workspace_image_suffix(url: &str, mutation: Option<&str>) -> String {
    mutation.map_or_else(String::new, |mutation| {
        let encoded: String = url::form_urlencoded::byte_serialize(mutation.as_bytes()).collect::<String>().replace('+', "%20");
        format!("{}workspace-revision={encoded}", if url.contains('?') { '&' } else { '?' })
    })
}

pub fn allows_local_media_fallback(resource: &AssetResource, error_tag: Option<&str>, different_environment: bool) -> bool {
    let AssetResource::MediaFile { path, .. } = resource else { return false };
    let extension = path.rfind('.').map(|index| &path[index..]).unwrap_or_default();
    different_environment
        && matches!(error_tag, Some("AssetWorkspaceAssetNotFoundError" | "AssetWorkspaceAssetInspectionError" | "AssetWorkspaceContextNotFoundError"))
        && (path.starts_with('/') || is_windows_absolute_path(path))
        && (image_mime_from_extension(extension).is_some() || video_mime_from_extension(extension).is_some())
}

pub fn resolve_asset_url(base: &str, relative: &str) -> Option<String> {
    let url = url::Url::parse(base).ok()?.join(relative).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownMediaRun {
    Prose(String),
    Image { url: String, alt: String },
}

pub fn markdown_media_runs(node: &markdown::mdast::Node, source: &str) -> Vec<MarkdownMediaRun> {
    use markdown::mdast::Node;
    let Node::Paragraph(paragraph) = node else { return Vec::new() };
    if !paragraph.children.iter().any(|node| matches!(node, Node::Image(_))) { return Vec::new(); }
    let Some(position) = &paragraph.position else { return Vec::new() };
    let mut cursor = position.start.offset;
    let mut runs = Vec::new();
    for child in &paragraph.children {
        if let Node::Image(image) = child && let Some(position) = &image.position {
            if let Some(prose) = source.get(cursor..position.start.offset).filter(|text| !text.is_empty()) {
                runs.push(MarkdownMediaRun::Prose(prose.to_owned()));
            }
            runs.push(MarkdownMediaRun::Image { url: image.url.clone(), alt: image.alt.clone() });
            cursor = position.end.offset;
        }
    }
    if let Some(prose) = source.get(cursor..position.end.offset).filter(|text| !text.is_empty()) {
        runs.push(MarkdownMediaRun::Prose(prose.to_owned()));
    }
    runs
}

pub struct MarkdownMediaDocument {
    source: std::sync::Arc<str>,
    definitions: std::sync::OnceLock<std::collections::HashMap<String, String>>,
}

impl MarkdownMediaDocument {
    pub fn new(source: std::sync::Arc<str>) -> Self { Self { source, definitions: Default::default() } }
    pub fn prose(&self, fragment: &str, references: &[String]) -> String {
        if references.is_empty() { return fragment.to_owned(); }
        let definitions = self.definitions.get_or_init(|| {
            let Ok(root) = markdown::to_mdast(&self.source, &markdown::ParseOptions::gfm()) else { return Default::default() };
            fn collect(node: &markdown::mdast::Node, source: &str, out: &mut std::collections::HashMap<String, String>) {
                if let markdown::mdast::Node::Definition(definition) = node {
                    if let Some(position) = &definition.position && let Some(text) = source.get(position.start.offset..position.end.offset) {
                        out.entry(definition.identifier.clone()).or_insert_with(|| text.to_owned());
                    }
                } else if let Some(children) = node.children() {
                    for child in children { collect(child, source, out); }
                }
            }
            let mut out = std::collections::HashMap::new(); collect(&root, &self.source, &mut out); out
        });
        let mut out = fragment.to_owned();
        for reference in references { if let Some(definition) = definitions.get(reference) { out.push_str("\n\n"); out.push_str(definition); } }
        out
    }
}

pub struct MediaVisibility<K> { consumers: std::collections::HashMap<u64, K> }
impl<K> Default for MediaVisibility<K> {
    fn default() -> Self { Self { consumers: Default::default() } }
}
impl<K: Eq + Clone> MediaVisibility<K> {
    pub fn set(&mut self, consumer: u64, key: Option<K>) -> bool {
        if self.consumers.get(&consumer) == key.as_ref() { return false; }
        if let Some(key) = key { self.consumers.insert(consumer, key); }
        else { self.consumers.remove(&consumer); }
        true
    }
    pub fn get(&self, consumer: u64) -> Option<&K> { self.consumers.get(&consumer) }
    pub fn contains(&self, key: &K) -> bool { self.consumers.values().any(|value| value == key) }
    pub fn iter(&self) -> impl Iterator<Item = (u64, &K)> { self.consumers.iter().map(|(id, key)| (*id, key)) }
    pub fn keys(&self) -> impl Iterator<Item = &K> { self.consumers.values() }
    pub fn is_empty(&self) -> bool { self.consumers.is_empty() }
    pub fn len(&self) -> usize { self.consumers.len() }
}

pub fn media_cache_evictions<K>(mut residents: Vec<(K, usize, u64, bool)>, budget: usize, max_entries: usize) -> Vec<K> {
    let mut count = residents.len();
    let mut bytes: usize = residents.iter().map(|(_, bytes, _, _)| bytes).sum();
    residents.sort_by_key(|(_, _, used, _)| *used);
    let mut evict = Vec::new();
    for (key, size, _, protected) in residents {
        if bytes <= budget && count <= max_entries { break; }
        if !protected { bytes -= size; count -= 1; evict.push(key); }
    }
    evict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{AssetResource, MarkdownImageSource, classify_markdown_image_source};

    #[test]
    fn viewport_consumers_release_resources_and_bound_inactive_decodes() {
        let mut visibility = MediaVisibility::default();
        assert!(visibility.set(1, Some("a")));
        assert!(!visibility.set(1, Some("a")));
        visibility.set(2, Some("a"));
        visibility.set(1, None);
        assert!(visibility.contains(&"a"));
        visibility.set(2, None);
        assert!(visibility.is_empty());
        let residents = (0..100).map(|key| (key, 10, key as u64, key == 99)).collect();
        let evicted = media_cache_evictions(residents, 20, 100);
        assert_eq!(evicted, (0..98).collect::<Vec<_>>());
        assert_eq!(media_cache_evictions((0..100).map(|key| (key, 1, key as u64, key == 99)).collect(), 1000, 2), (0..98).collect::<Vec<_>>());
    }

    #[test]
    fn file_images_resolve_beside_the_document_on_both_host_platforms() {
        for (cwd, path, expected) in [
            ("/work", "docs/readme.md", "/work/docs/../art/a.png"),
            ("/work", "/else/docs/readme.md", "/else/docs/../art/a.png"),
            ("C:\\work", "docs\\readme.md", "C:\\work\\docs\\..\\art\\a.png"),
            ("/work", "README.md", "/work/../art/a.png"),
            ("C:\\work", "C:\\README.md", "C:\\..\\art\\a.png"),
        ] {
            let base = markdown_image_base_dir(cwd, path);
            assert_eq!(classify_markdown_image_source("../art/a.png", Some(&base)), MarkdownImageSource::WorkspaceFile(expected.into()));
        }
    }

    #[test]
    fn first_workspace_edit_invalidates_an_open_preview_but_switching_roots_does_not() {
        assert!(workspace_mutation_changed(false, None, Some("first")));
        assert!(workspace_mutation_changed(false, Some("first"), None));
        assert!(!workspace_mutation_changed(false, None, None));
        assert!(!workspace_mutation_changed(true, None, Some("first")));
        assert_eq!(workspace_image_suffix("/a?token=x", Some("edit /1")), "&workspace-revision=edit%20%2F1");
        assert_eq!(workspace_image_suffix("/a", None), "");
    }

    #[test]
    fn local_fallback_is_only_for_missing_absolute_media_on_another_environment() {
        let resource = |path: &str| AssetResource::MediaFile { thread_id: "t".into(), path: path.into() };
        for tag in ["AssetWorkspaceAssetNotFoundError", "AssetWorkspaceAssetInspectionError", "AssetWorkspaceContextNotFoundError"] {
            assert!(allows_local_media_fallback(&resource("/tmp/a.png"), Some(tag), true));
            assert!(allows_local_media_fallback(&resource("C:\\temp\\a.mp4"), Some(tag), true));
            for path in ["relative.png", "/tmp/a.txt", "/tmp/no-extension"] {
                assert!(!allows_local_media_fallback(&resource(path), Some(tag), true));
            }
        }
        assert!(!allows_local_media_fallback(&resource("/a.png"), Some("Forbidden"), true));
        assert!(!allows_local_media_fallback(&resource("/a.png"), Some("AssetWorkspaceAssetNotFoundError"), false));
        assert!(!allows_local_media_fallback(&AssetResource::Attachment { attachment_id: "a".into() }, Some("AssetWorkspaceAssetNotFoundError"), true));
    }

    #[test]
    fn remount_stales_at_five_minutes_and_mounted_refresh_is_thirty_minutes() {
        assert_eq!(asset_refresh_delay(Some(0.), 299_999., true), Some(1_500_001.));
        assert!(asset_is_stale(Some(0.), 300_000.));
        assert!(!asset_is_stale(Some(0.), 299_999.));
        assert_eq!(asset_refresh_delay(Some(0.), 1_800_000., true), Some(0.));
        assert_eq!(asset_refresh_delay(Some(0.), 1_800_000., false), None);
        assert_eq!(asset_refresh_delay(None, 0., true), Some(1_800_000.));
        assert_eq!(asset_refresh_schedule(None, 10., Some(100.)), Some((110., 100.)));
        assert_eq!(asset_refresh_schedule(Some(110.), 10., Some(200.)), None);
        assert_eq!(asset_refresh_schedule(Some(2000.), 0., Some(500.)), Some((500., 500.)));
        assert_eq!(asset_refresh_schedule(Some(2000.), 0., Some(1000.)), None);
        assert_eq!(asset_refresh_schedule(None, 0., None), None);
    }
}

#[cfg(test)]
mod paragraph_tests {
    use super::*;
    #[test]
    fn prose_images_use_the_media_path_without_dropping_surrounding_markdown() {
        let text = "Before **bold** ![chart](img/a.png) and ![two](b.svg) after.";
        let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
        let paragraph = &root.children().unwrap()[0];
        assert_eq!(markdown_media_runs(paragraph, text), vec![
            MarkdownMediaRun::Prose("Before **bold** ".into()),
            MarkdownMediaRun::Image { url: "img/a.png".into(), alt: "chart".into() },
            MarkdownMediaRun::Prose(" and ".into()),
            MarkdownMediaRun::Image { url: "b.svg".into(), alt: "two".into() },
            MarkdownMediaRun::Prose(" after.".into()),
        ]);
        let document = MarkdownMediaDocument::new("See [guide][g] ![chart](chart.png)\n\n[g]: https://example.com".into());
        let fragment = "See [guide][g] ";
        assert!(document.prose(fragment, &["g".into()]).contains("[g]: https://example.com"));
        assert!(document.prose(fragment, &["g".into()]).starts_with(fragment));
        assert_eq!(resolve_asset_url("http://host:1/base", "/api/assets/a"), Some("http://host:1/api/assets/a".into()));
        assert_eq!(resolve_asset_url("http://host:1", "https://local:2/api/assets/a"), Some("https://local:2/api/assets/a".into()));
        assert_eq!(resolve_asset_url("http://host:1", "javascript:bad"), None);
    }
}
