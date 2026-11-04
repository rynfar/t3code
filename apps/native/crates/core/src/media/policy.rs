//! Ports `state/assets.ts`, `files/FileMarkdownPreview.tsx`, and the paragraph image/link
//! behavior of `ChatMarkdown.tsx`, `chat/ChatMarkdownMedia.tsx`, and `chat/markdownRenderer.ts`.

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
    Image { url: String, alt: String, link: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownImageAction<'a> {
    Preview,
    OpenLink(&'a str),
    PreviewLink(&'a str),
    Inert,
}

pub fn markdown_external_link(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// The anchor owns a linked image's click, including when its destination is blocked.
pub fn markdown_image_action(link: Option<&str>, modified: bool) -> MarkdownImageAction<'_> {
    let Some(link) = link else { return MarkdownImageAction::Preview };
    if !markdown_external_link(link) { return MarkdownImageAction::Inert; }
    if !modified && let Ok(url) = url::Url::parse(link) && matches!(url.scheme(), "http" | "https") {
        let extension = url.path().rfind('.').map(|at| &url.path()[at..]).unwrap_or_default();
        if image_mime_from_extension(extension).is_some() { return MarkdownImageAction::PreviewLink(link); }
    }
    MarkdownImageAction::OpenLink(link)
}

struct MarkdownDefinition { source: String, url: String }

pub struct MarkdownMediaDocument {
    source: std::sync::Arc<str>,
    definitions: std::sync::OnceLock<std::collections::HashMap<String, MarkdownDefinition>>,
}

fn node_source<'a>(node: &markdown::mdast::Node, source: &'a str) -> Option<&'a str> {
    let position = node.position()?;
    source.get(position.start.offset..position.end.offset)
}

fn push_run(runs: &mut Vec<MarkdownMediaRun>, run: MarkdownMediaRun) {
    if let MarkdownMediaRun::Prose(text) = &run {
        if text.is_empty() { return; }
        if let Some(MarkdownMediaRun::Prose(previous)) = runs.last_mut() { previous.push_str(text); return; }
    }
    runs.push(run);
}

fn wrap_prose(text: &str, prefix: &str, suffix: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() { return text.to_owned(); }
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    format!("{}{prefix}{trimmed}{suffix}{}", &text[..start], &text[end..])
}

impl MarkdownMediaDocument {
    pub fn new(source: std::sync::Arc<str>) -> Self { Self { source, definitions: Default::default() } }

    fn definitions(&self) -> &std::collections::HashMap<String, MarkdownDefinition> {
        self.definitions.get_or_init(|| {
            let Ok(root) = markdown::to_mdast(&self.source, &markdown::ParseOptions::gfm()) else { return Default::default() };
            fn collect(node: &markdown::mdast::Node, source: &str, out: &mut std::collections::HashMap<String, MarkdownDefinition>) {
                if let markdown::mdast::Node::Definition(definition) = node {
                    if let Some(text) = node_source(node, source) {
                        out.entry(definition.identifier.clone()).or_insert_with(|| MarkdownDefinition { source: text.to_owned(), url: definition.url.clone() });
                    }
                } else if let Some(children) = node.children() {
                    for child in children { collect(child, source, out); }
                }
            }
            let mut out = std::collections::HashMap::new(); collect(&root, &self.source, &mut out); out
        })
    }

    // Appending only the last parsed block cannot update earlier references when definitions arrive.
    pub fn may_have_definitions(&self) -> bool { self.source.contains("]:") }

    pub fn prose(&self, fragment: &str, references: &[String]) -> String {
        if references.is_empty() { return fragment.to_owned(); }
        let definitions = self.definitions();
        let mut out = fragment.to_owned();
        let mut seen = std::collections::HashSet::new();
        for reference in references {
            if seen.insert(reference) && let Some(definition) = definitions.get(reference) { out.push_str("\n\n"); out.push_str(&definition.source); }
        }
        out
    }

    pub fn runs(&self, node: &markdown::mdast::Node, source: &str) -> Vec<MarkdownMediaRun> {
        if !matches!(node, markdown::mdast::Node::Paragraph(_)) { return Vec::new(); }
        let runs = self.split(node, source);
        if !runs.iter().any(|run| matches!(run, MarkdownMediaRun::Image { .. })) { return Vec::new(); }
        let mut refs = Vec::new();
        fn references(node: &markdown::mdast::Node, refs: &mut Vec<String>) {
            if let markdown::mdast::Node::LinkReference(link) = node { refs.push(link.identifier.clone()); }
            if let Some(children) = node.children() { for child in children { references(child, refs); } }
        }
        references(node, &mut refs);
        runs.into_iter().map(|run| match run {
            MarkdownMediaRun::Prose(text) if !text.trim().is_empty() => MarkdownMediaRun::Prose(self.prose(&text, &refs)),
            run => run,
        }).collect()
    }

    fn split(&self, node: &markdown::mdast::Node, source: &str) -> Vec<MarkdownMediaRun> {
        use markdown::mdast::Node;
        let original = node_source(node, source).unwrap_or_default();
        let image = match node {
            Node::Image(image) => Some((image.url.clone(), image.alt.clone())),
            Node::ImageReference(image) => self.definitions().get(&image.identifier).map(|definition| (definition.url.clone(), image.alt.clone())),
            _ => None,
        };
        if let Some((url, alt)) = image { return vec![MarkdownMediaRun::Image { url, alt, link: None }]; }
        if !matches!(node, Node::Paragraph(_) | Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_) | Node::Link(_) | Node::LinkReference(_)) {
            return vec![MarkdownMediaRun::Prose(original.into())];
        }
        let children = node.children().unwrap();
        let mut runs = Vec::new();
        for child in children { for run in self.split(child, source) { push_run(&mut runs, run); } }
        if !runs.iter().any(|run| matches!(run, MarkdownMediaRun::Image { .. })) { return vec![MarkdownMediaRun::Prose(original.into())]; }
        let link = match node {
            Node::Link(link) => Some(link.url.clone()),
            Node::LinkReference(link) => Some(self.definitions().get(&link.identifier).map(|definition| definition.url.clone()).unwrap_or_default()),
            _ => None,
        };
        if !matches!(node, Node::Paragraph(_)) {
            let position = node.position().unwrap();
            let first = children.first().and_then(Node::position).unwrap();
            let last = children.last().and_then(Node::position).unwrap();
            let prefix = source.get(position.start.offset..first.start.offset).unwrap_or_default();
            let suffix = source.get(last.end.offset..position.end.offset).unwrap_or_default();
            for run in &mut runs {
                match run {
                    MarkdownMediaRun::Prose(text) => *text = wrap_prose(text, prefix, suffix),
                    MarkdownMediaRun::Image { link: owner, .. } => if link.is_some() { *owner = link.clone(); },
                }
            }
        }
        runs
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
    fn runs(text: &str) -> Vec<MarkdownMediaRun> {
        let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
        MarkdownMediaDocument::new(text.into()).runs(&root.children().unwrap()[0], text)
    }

    #[test]
    fn nested_images_preserve_balanced_prose_and_link_ownership() {
        let text = "Before **bold _left ![chart](a.png) right_ end** after";
        let result = runs(text);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], MarkdownMediaRun::Prose("Before **bold _left_** ".into()));
        assert_eq!(result[2], MarkdownMediaRun::Prose(" **_right_ end** after".into()));
        // Link ownership is checked without invoking an opener.
        assert_eq!(runs("[![chart](a.png)](https://example.com)"), vec![MarkdownMediaRun::Image { url: "a.png".into(), alt: "chart".into(), link: Some("https://example.com".into()) }]);
    }

    #[test]
    fn reference_images_use_first_definition_and_unknown_references_stay_literal() {
        for image in ["![label][PiC]", "![pic][]", "![pic]"] {
            let text = format!("before {image} after\n\n[pic]: first.png \"title\"\n[pic]: second.png");
            let result = runs(&text);
            assert_eq!(result.len(), 3, "{image}: {result:?}");
            assert!(matches!(&result[1], MarkdownMediaRun::Image { url, .. } if url == "first.png"));
        }
        assert!(runs("before ![missing][unknown] after").is_empty());
        assert!(runs("`![code](a.png)`").is_empty());
    }

    #[test]
    fn linked_reference_images_keep_the_anchor_and_first_prose_definition() {
        let result = runs("[**left ![chart][pic] right**][guide]\n\n[pic]: local.png\n[guide]: https://example.com/first\n[guide]: javascript:bad");
        assert_eq!(result, vec![
            MarkdownMediaRun::Prose("[**left**][guide] \n\n[guide]: https://example.com/first".into()),
            MarkdownMediaRun::Image { url: "local.png".into(), alt: "chart".into(), link: Some("https://example.com/first".into()) },
            MarkdownMediaRun::Prose(" [**right**][guide]\n\n[guide]: https://example.com/first".into()),
        ]);
        let result = runs("*![chart][A B]*\n\n[a  b]: javascript:bad\n[a b]: good.png");
        assert!(matches!(&result[0], MarkdownMediaRun::Image { url, .. } if url == "javascript:bad"));
        let result = runs("![known](a.png) and ![missing][unknown] plus `![code](a.png)`");
        assert_eq!(result[1], MarkdownMediaRun::Prose(" and ![missing][unknown] plus `![code](a.png)`".into()));
        assert!(runs("~~before ![x](a.png) after~~").iter().any(|run| matches!(run, MarkdownMediaRun::Prose(text) if text == "~~before~~ ")));
    }

    #[test]
    fn anchor_decisions_never_preview_the_child_or_open_local_and_unsafe_links() {
        assert_eq!(markdown_image_action(None, false), MarkdownImageAction::Preview);
        for url in ["docs/a.md", "/a.png", "file:///a.png", "javascript:bad", "data:image/png,abc", "#section", "", "//host/a.png"] {
            assert_eq!(markdown_image_action(Some(url), false), MarkdownImageAction::Inert, "{url}");
        }
        for url in ["https://example.com/guide", "http://host/page", "mailto:team@example.com"] {
            assert_eq!(markdown_image_action(Some(url), false), MarkdownImageAction::OpenLink(url));
        }
        let url = "https://example.com/target.png?x=1#details";
        assert_eq!(markdown_image_action(Some(url), false), MarkdownImageAction::PreviewLink(url));
        assert_eq!(markdown_image_action(Some(url), true), MarkdownImageAction::OpenLink(url));
    }

    #[test]
    fn prose_images_use_the_media_path_without_dropping_surrounding_markdown() {
        let text = "Before **bold** ![chart](img/a.png) and ![two](b.svg) after.";
        let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
        let paragraph = &root.children().unwrap()[0];
        assert_eq!(MarkdownMediaDocument::new(text.into()).runs(paragraph, text), vec![
            MarkdownMediaRun::Prose("Before **bold** ".into()),
            MarkdownMediaRun::Image { url: "img/a.png".into(), alt: "chart".into(), link: None },
            MarkdownMediaRun::Prose(" and ".into()),
            MarkdownMediaRun::Image { url: "b.svg".into(), alt: "two".into(), link: None },
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
