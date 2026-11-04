//! Ports `state/assets.ts`, `files/FileMarkdownPreview.tsx`, and the paragraph/heading image/link
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

#[derive(Clone, PartialEq, Eq)]
struct MarkdownDefinition { source: String, url: String, title: Option<String>, start: usize }

#[derive(Default)]
struct MarkdownMediaIndex {
    definitions: std::sync::Arc<std::collections::HashMap<String, MarkdownDefinition>>,
    paragraphs: std::collections::HashMap<usize, std::sync::Arc<Vec<MarkdownMediaRun>>>,
    heading_gaps: std::collections::HashMap<usize, bool>,
    tail: usize,
    multiline_titles: Vec<(usize, usize)>,
    #[cfg(test)]
    parsed_from: usize,
}

pub struct MarkdownMediaDocument {
    source: std::sync::Arc<str>,
    index: std::sync::OnceLock<MarkdownMediaIndex>,
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

fn escaped_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() { if ch.is_ascii_punctuation() { out.push('\\'); } out.push(ch); }
    out
}

pub fn heading_literal_prefix<'a>(node: &markdown::mdast::Node, source: &'a str) -> &'a str {
    if !matches!(node, markdown::mdast::Node::Heading(_)) { return ""; }
    let Some((position, first)) = node.position().zip(node.children().and_then(|children| children.first()).and_then(markdown::mdast::Node::position)) else { return "" };
    let prefix = source.get(position.start.offset..first.start.offset).unwrap_or_default().trim_start_matches([' ', '\t']);
    if !prefix.starts_with('#') { return ""; }
    prefix.trim_start_matches('#').trim_start_matches([' ', '\t'])
}

fn inline_source(node: &markdown::mdast::Node, source: &str, heading: bool) -> String {
    let raw = node_source(node, source).unwrap_or_default();
    match node {
        markdown::mdast::Node::Text(text) if heading => escaped_text(&text.value).split_inclusive('\n').map(|line| {
            let indent: String = line.chars().take_while(|ch| matches!(ch, ' ' | '\t')).collect();
            if indent.len() >= 4 || indent.contains('\t') {
                let first = line.chars().next().unwrap();
                format!("&#{};{}", first as u32, &line[first.len_utf8()..])
            } else { line.to_owned() }
        }).collect(),
        markdown::mdast::Node::Text(text) if raw != text.value => escaped_text(&text.value),
        markdown::mdast::Node::InlineCode(code) if raw.contains('\n') => {
            let fence = "`".repeat(code.value.split(|ch| ch != '`').map(str::len).max().unwrap_or(0) + 1);
            format!("{fence} {} {fence}", code.value)
        }
        _ => raw.to_owned(),
    }
}

fn markdown_destination(url: &str, title: Option<&str>) -> String {
    let url = url.replace('&', "&amp;").replace('\\', "\\\\").replace('>', "\\>").replace('<', "\\<");
    let title = title.map(|title| format!(" \"{}\"", title.replace('&', "&amp;").replace('\\', "\\\\").replace('"', "\\\""))).unwrap_or_default();
    format!("<{url}>{title}")
}

fn same_definitions(a: &std::collections::HashMap<String, MarkdownDefinition>, b: &std::collections::HashMap<String, MarkdownDefinition>) -> bool {
    a.len() == b.len() && a.iter().all(|(id, a)| b.get(id).is_some_and(|b| a.url == b.url && a.title == b.title))
}

impl MarkdownMediaIndex {
    fn parse(source: &str, start: usize, previous: Option<&Self>) -> Self {
        use markdown::mdast::Node;
        if source.is_empty() { return Self::default(); }
        let mut definitions: std::collections::HashMap<_, _> = previous.into_iter().flat_map(|index| index.definitions.iter())
            .filter(|(_, definition)| definition.start < start).map(|(id, definition)| (id.clone(), definition.clone())).collect();
        let mut multiline_titles: Vec<_> = previous.into_iter().flat_map(|index| index.multiline_titles.iter()).filter(|(position, _)| *position < start).copied().collect();
        let mut fragment = source[start..].to_owned();
        let real_end = fragment.len();
        for definition in definitions.values() { fragment.push_str("\n\n"); fragment.push_str(&definition.source); }
        let Ok(root) = markdown::to_mdast(&fragment, &markdown::ParseOptions::gfm()) else { return Self::default() };
        fn collect(node: &Node, source: &str, start: usize, end: usize, out: &mut std::collections::HashMap<String, MarkdownDefinition>, titles: &mut Vec<(usize, usize)>) {
            if let Node::Definition(definition) = node {
                if let Some(position) = &definition.position && position.start.offset < end && let Some(text) = node_source(node, source) {
                    if definition.title.is_some() && position.start.line < position.end.line { titles.push((start + position.start.offset, start + position.end.offset)); }
                    out.entry(definition.identifier.clone()).or_insert_with(|| MarkdownDefinition {
                        source: if text.contains('\n') { format!("[{}]: {}", definition.identifier, markdown_destination(&definition.url, definition.title.as_deref())) } else { text.to_owned() },
                        url: definition.url.clone(), title: definition.title.clone(), start: start + position.start.offset,
                    });
                }
            } else if let Some(children) = node.children() { for child in children { collect(child, source, start, end, out, titles); } }
        }
        collect(&root, &fragment, start, real_end, &mut definitions, &mut multiline_titles);
        // A changed effective definition can resolve an earlier literal reference, or change its target.
        if start > 0 && previous.is_some_and(|previous| !same_definitions(&previous.definitions, &definitions)) {
            return Self::parse(source, 0, None);
        }
        let definitions = match previous.filter(|previous| *previous.definitions == definitions) {
            Some(previous) => previous.definitions.clone(),
            None => std::sync::Arc::new(definitions),
        };
        let mut paragraphs: std::collections::HashMap<_, _> = previous.into_iter().flat_map(|index| index.paragraphs.iter())
            .filter(|(position, _)| **position < start).map(|(position, runs)| (*position, runs.clone())).collect();
        let mut heading_gaps: std::collections::HashMap<_, _> = previous.into_iter().flat_map(|index| index.heading_gaps.iter())
            .filter(|(position, _)| **position < start).map(|(position, gap)| (*position, *gap)).collect();
        let parser = MarkdownMediaParser { definitions: &definitions };
        fn paragraphs_in(node: &Node, parser: &MarkdownMediaParser<'_>, source: &str, range: (usize, usize), last: bool, out: &mut std::collections::HashMap<usize, std::sync::Arc<Vec<MarkdownMediaRun>>>, gaps: &mut std::collections::HashMap<usize, bool>) {
            let (start, end) = range;
            if matches!(node, Node::Paragraph(_) | Node::Heading(_)) && let Some(position) = node.position() && position.start.offset < end {
                let runs = parser.runs(node, source);
                if !runs.is_empty() {
                    out.insert(start + position.start.offset, std::sync::Arc::new(runs));
                    if matches!(node, Node::Heading(_)) { gaps.insert(start + position.start.offset, !last); }
                }
            } else if let Some(children) = node.children() {
                let mut children = children.iter().filter(|child| child.position().is_some_and(|position| position.start.offset < end)).peekable();
                while let Some(child) = children.next() {
                    // TextView renders each direct list-item block as last, including headings.
                    let last = matches!(node, Node::ListItem(_)) || children.peek().is_none();
                    paragraphs_in(child, parser, source, range, last, out, gaps);
                }
            }
        }
        paragraphs_in(&root, &parser, &fragment, (start, real_end), true, &mut paragraphs, &mut heading_gaps);
        let mut blocks = root.children().into_iter().flatten().filter(|node| node.position().is_some_and(|position| position.start.offset < real_end)).rev();
        let last = blocks.next();
        let mut tail = last.and_then(Node::position).map_or(source.len(), |position| start + position.start.offset);
        // An adjacent unfinished title can still attach to the preceding definition.
        if let (Some(Node::Paragraph(paragraph)), Some(Node::Definition(definition))) = (last, blocks.next())
            && definition.title.is_none()
            && let (Some(paragraph), Some(definition)) = (&paragraph.position, &definition.position)
            && paragraph.start.line == definition.end.line + 1
            && fragment.get(paragraph.start.offset..paragraph.end.offset).is_some_and(|text| text.trim_start().starts_with(['"', '\'', '(']))
        {
            tail = start + definition.start.offset;
        }
        Self { definitions, paragraphs, heading_gaps, tail, multiline_titles, #[cfg(test)] parsed_from: start }
    }
}

impl MarkdownMediaDocument {
    pub fn new(source: std::sync::Arc<str>) -> Self { Self { source, index: Default::default() } }
    fn index(&self) -> &MarkdownMediaIndex { self.index.get_or_init(|| MarkdownMediaIndex::parse(&self.source, 0, None)) }
    pub fn prepare(self) -> Self { self.index(); self }
    pub fn source(&self) -> &std::sync::Arc<str> { &self.source }

    /// Called on the background worker; completed paragraphs, headings and reference definitions are retained.
    pub fn updated(&self, source: std::sync::Arc<str>) -> Self {
        let previous = self.index();
        let index = if source.starts_with(&*self.source) { MarkdownMediaIndex::parse(&source, previous.tail, Some(previous)) }
            else { MarkdownMediaIndex::parse(&source, 0, None) };
        Self { source, index: std::sync::OnceLock::from(index) }
    }

    pub fn append_delta<'a>(&self, next: &'a Self) -> Option<&'a str> {
        if self.source.is_empty() || !same_definitions(&self.index().definitions, &next.index().definitions) { return None; }
        // Even an ignored duplicate's completed title can consume a previously visible paragraph.
        if next.index().multiline_titles.iter().any(|(start, end)| *start < self.source.len() && *end > self.source.len()) { return None; }
        next.source.strip_prefix(&*self.source)
    }

    pub fn prose(&self, fragment: &str, references: &[String]) -> String {
        MarkdownMediaParser { definitions: &self.index().definitions }.prose(fragment, references)
    }

    pub fn prose_run_at(&self, start: usize, run: usize) -> Option<&str> {
        match self.index().paragraphs.get(&start)?.get(run)? {
            MarkdownMediaRun::Prose(text) => Some(text),
            _ => None,
        }
    }

    pub fn heading_has_paragraph_gap(&self, node: &markdown::mdast::Node, offset: usize) -> bool {
        node.position().and_then(|position| self.index().heading_gaps.get(&(offset + position.start.offset))).copied().unwrap_or(false)
    }

    pub fn runs_at(&self, node: &markdown::mdast::Node, offset: usize) -> Vec<MarkdownMediaRun> {
        if !matches!(node, markdown::mdast::Node::Paragraph(_) | markdown::mdast::Node::Heading(_)) { return Vec::new(); }
        node.position().and_then(|position| self.index().paragraphs.get(&(offset + position.start.offset))).map(|runs| (**runs).clone()).unwrap_or_default()
    }
}

struct MarkdownMediaParser<'a> { definitions: &'a std::collections::HashMap<String, MarkdownDefinition> }

impl MarkdownMediaParser<'_> {
    pub fn prose(&self, fragment: &str, references: &[String]) -> String {
        if references.is_empty() { return fragment.to_owned(); }
        let definitions = self.definitions;
        let mut out = fragment.to_owned();
        let mut seen = std::collections::HashSet::new();
        for reference in references {
            if seen.insert(reference) && let Some(definition) = definitions.get(reference) { out.push_str("\n\n"); out.push_str(&definition.source); }
        }
        out
    }

    pub fn runs(&self, node: &markdown::mdast::Node, source: &str) -> Vec<MarkdownMediaRun> {
        if !matches!(node, markdown::mdast::Node::Paragraph(_) | markdown::mdast::Node::Heading(_)) { return Vec::new(); }
        let runs = self.split(node, source, matches!(node, markdown::mdast::Node::Heading(_)));
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

    fn split(&self, node: &markdown::mdast::Node, source: &str, heading: bool) -> Vec<MarkdownMediaRun> {
        use markdown::mdast::Node;
        let original = inline_source(node, source, heading);
        let image = match node {
            Node::Image(image) => Some((image.url.clone(), image.alt.clone())),
            Node::ImageReference(image) => self.definitions.get(&image.identifier).map(|definition| (definition.url.clone(), image.alt.clone())),
            _ => None,
        };
        if let Some((url, alt)) = image { return vec![MarkdownMediaRun::Image { url, alt, link: None }]; }
        if !matches!(node, Node::Paragraph(_) | Node::Heading(_) | Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_) | Node::Link(_) | Node::LinkReference(_)) {
            return vec![MarkdownMediaRun::Prose(original)];
        }
        let children = node.children().unwrap();
        if children.is_empty() { return vec![MarkdownMediaRun::Prose(original)]; }
        let mut runs = Vec::new();
        // markdown-rs consumes repeated opening ATX groups; the later groups are authored inline text.
        push_run(&mut runs, MarkdownMediaRun::Prose(escaped_text(heading_literal_prefix(node, source))));
        for child in children { for run in self.split(child, source, heading) { push_run(&mut runs, run); } }
        let link = match node {
            Node::Link(link) => Some(link.url.clone()),
            Node::LinkReference(link) => Some(self.definitions.get(&link.identifier).map(|definition| definition.url.clone()).unwrap_or_default()),
            _ => None,
        };
        if !matches!(node, Node::Paragraph(_) | Node::Heading(_)) {
            let position = node.position().unwrap();
            let first = children.first().and_then(Node::position).unwrap();
            let last = children.last().and_then(Node::position).unwrap();
            let prefix = source.get(position.start.offset..first.start.offset).unwrap_or_default();
            let suffix = source.get(last.end.offset..position.end.offset).unwrap_or_default();
            let autolink = heading && matches!(node, Node::Link(_)) && !prefix.starts_with('[');
            let prefix = if autolink { "[" } else { prefix };
            let suffix = match node {
                Node::Link(link) if autolink || suffix.contains('\n') => format!("]({})", markdown_destination(&link.url, link.title.as_deref())),
                // Shortcut/collapsed display labels can decode differently from their reference identifiers.
                Node::LinkReference(link) if link.reference_kind != markdown::mdast::ReferenceKind::Full || suffix.contains('\n') => {
                    self.definitions.get(&link.identifier).map_or_else(|| suffix.to_owned(), |definition| format!("]({})", markdown_destination(&definition.url, definition.title.as_deref())))
                }
                _ => suffix.to_owned(),
            };
            for run in &mut runs {
                match run {
                    MarkdownMediaRun::Prose(text) => *text = wrap_prose(text, prefix, &suffix),
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
        MarkdownMediaDocument::new(text.into()).runs_at(&root.children().unwrap()[0], 0)
    }

    #[test]
    fn streaming_body_appends_do_not_invalidate_reference_context() {
        let text = "![x][pic]\n\n[pic]: a.png\n\nBody.\n\nTail";
        let document = MarkdownMediaDocument::new(text.into()).prepare();
        let next = document.updated(format!("{text} more body").into());
        assert_eq!(document.append_delta(&next), Some(" more body"));
        assert_eq!(next.index().parsed_from, text.rfind("Tail").unwrap());
        assert!(std::sync::Arc::ptr_eq(&document.index().definitions, &next.index().definitions));
        let literal = MarkdownMediaDocument::new("`]:` ![x](a.png)\n\n```md\n[pic]: fake.png\n```\n\nTail".into()).prepare();
        let next = literal.updated(format!("{} body", literal.source()).into());
        assert_eq!(literal.append_delta(&next), Some(" body"));
        assert!(next.index().definitions.is_empty());
        assert!(std::sync::Arc::ptr_eq(&literal.index().definitions, &next.index().definitions));
        let missing = MarkdownMediaDocument::new("![x][pic]\n\n[pi".into()).prepare();
        let split = missing.updated("![x][pic]\n\n[pic]:".into());
        assert_eq!(missing.append_delta(&split), Some("c]:"));
        let resolved = split.updated("![x][pic]\n\n[pic]: a.png\n\n".into());
        assert!(split.append_delta(&resolved).is_none());
        assert_eq!(resolved.index().parsed_from, 0);
        assert!(matches!(&resolved.index().paragraphs[&0][0], MarkdownMediaRun::Image { url, .. } if url == "a.png"));
        let body = resolved.updated(format!("{}More ![x][pic]", resolved.source()).into());
        assert_eq!(resolved.append_delta(&body), Some("More ![x][pic]"));
        assert_eq!(body.index().paragraphs.len(), 2);
        let changed = body.updated("![x][pic]\n\n[pic]: b.png".into());
        assert!(body.append_delta(&changed).is_none());
        assert!(matches!(&changed.index().paragraphs[&0][0], MarkdownMediaRun::Image { url, .. } if url == "b.png"));
        let removed = changed.updated("![x][pic]".into());
        assert!(changed.append_delta(&removed).is_none());
        assert!(removed.index().paragraphs.is_empty());
        let duplicate = body.updated(format!("{}\n\n[pic]: ignored.png", body.source()).into());
        assert!(body.append_delta(&duplicate).is_some());
        assert!(std::sync::Arc::ptr_eq(&body.index().definitions, &duplicate.index().definitions));
        for indent in ["", "  ", "    ", "\t"] {
            for (open, close) in [("\"title", "\""), ("'title", "'"), ("(title", ")")] {
                let text = format!("![x][p]\n\n[p]: a.png\n{indent}{open}");
                let before = MarkdownMediaDocument::new(text.clone().into()).prepare();
                let complete = format!("{text}{close}\n\n");
                let after = before.updated(complete.clone().into());
                let full = MarkdownMediaDocument::new(complete.into()).prepare();
                assert!(same_definitions(&after.index().definitions, &full.index().definitions), "{text}");
                assert_eq!(after.index().definitions["p"].title.as_deref(), Some("title"));
                assert_eq!(after.index().paragraphs, full.index().paragraphs);
                assert!(before.append_delta(&after).is_none());
                let body = after.updated(format!("{}Body", after.source()).into());
                assert_eq!(after.append_delta(&body), Some("Body"));
            }
        }
        let text = "Before ![x][p] after.\n\n[p]: a.png\n\n[p]: b.png\n  \"title";
        let before = MarkdownMediaDocument::new(text.into()).prepare();
        let after = before.updated(format!("{text}\"\n\n").into());
        assert!(same_definitions(&before.index().definitions, &after.index().definitions));
        assert!(before.append_delta(&after).is_none());
        let body = after.updated(format!("{}Body", after.source()).into());
        assert_eq!(after.append_delta(&body), Some("Body"));
        for text in [
            "![x][p]\n\n[p]: a.png\n  \"first\nsecond\"\n\nBody",
            "![x][p]\r\n\r\n[p]: a.png\r\n  'title'\r\n\r\nBody",
            "![x][p]\n\n[p]: a.png\n\n  \"separate prose\"\n\nBody",
            "![x][p]\n\n[p]: a.png\n  \"title\"\n\n[p]: b.png \"ignored\"\n\nBody",
            "![x][p]\n\n[p]: a.png\n\n[p]: b.png\n  \"ignored\"\n\nBody",
        ] {
            let mut previous = MarkdownMediaDocument::new("".into()).prepare();
            for end in text.char_indices().map(|(offset, ch)| offset + ch.len_utf8()) {
                let next = previous.updated(text[..end].into());
                let full = MarkdownMediaDocument::new(text[..end].into()).prepare();
                assert!(same_definitions(&next.index().definitions, &full.index().definitions), "{end}: {text:?}");
                assert_eq!(next.index().paragraphs, full.index().paragraphs, "{end}: {text:?}");
                previous = next;
            }
        }
    }

    #[test]
    fn nested_images_preserve_balanced_prose_and_link_ownership() {
        let text = "Before **bold _left ![chart](a.png) right_ end** after";
        let result = runs(text);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], MarkdownMediaRun::Prose("Before **bold _left_** ".into()));
        assert_eq!(result[2], MarkdownMediaRun::Prose(" **_right_ end** after".into()));
        fn prose_html(node: &markdown::mdast::Node, doc: &MarkdownMediaDocument) -> String {
            let result = doc.runs_at(node, 0);
            if !result.is_empty() { return result.into_iter().filter_map(|run| match run { MarkdownMediaRun::Prose(text) => Some(markdown::to_html_with_options(&text, &markdown::Options::gfm()).unwrap()), _ => None }).collect(); }
            node.children().map(|children| children.iter().map(|node| prose_html(node, doc)).collect()).unwrap_or_default()
        }
        for text in ["> **left\n> ![x](a.png)\n> right**", "> - **left\n>   ![x](a.png)\n>   right**", "> > **left\n> > ![x](a.png)\n> > right**"] {
            let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
            let html = prose_html(&root, &MarkdownMediaDocument::new(text.into()));
            assert_eq!(html.replace('\n', ""), "<p><strong>left</strong></p><p><strong>right</strong></p>", "{text}");
        }
        for (text, expected) in [
            ("> **left\n> ![x](a.png)\n> right &amp; \\*literal\\***", "<p><strong>left</strong></p><p><strong>right &amp; *literal*</strong></p>"),
            ("> left ![x](a.png) `code\n> right`", "<p>left</p><p><code>code right</code></p>"),
            ("> ![x](a.png)\n> **left\n> right**", "<p><strong>leftright</strong></p>"),
            ("> [left ![x](a.png) right](\n> https://example.com\n> )", "<p><a href=\"https://example.com\">left</a></p><p><a href=\"https://example.com\">right</a></p>"),
        ] {
            let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
            assert_eq!(prose_html(&root, &MarkdownMediaDocument::new(text.into())).replace('\n', ""), expected, "{text}");
        }
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
    fn heading_images_keep_inline_formatting_references_and_anchor_ownership() {
        let body = "Before **bold _left ![chart][pic] right_ end** [guide][g]";
        let definitions = "\n\n[pic]: docs/chart.png\n[g]: https://example.com";
        let expected = runs(&format!("{body}{definitions}"));
        assert!(!expected.is_empty());
        for heading in (1..=6).map(|level| format!("{} {body} ###", "#".repeat(level)))
            .chain([format!("{body}\n==="), format!("{body}\n---")])
        {
            assert_eq!(runs(&format!("{heading}{definitions}")), expected, "{heading}");
        }
        for link in ["https://example.com/target.png", "javascript:bad", "docs/local.png"] {
            assert_eq!(runs(&format!("## [left ![chart](a.png) right]({link})")), runs(&format!("[left ![chart](a.png) right]({link})")));
        }
        for text in ["## Plain **heading**", "## `![code](a.png)`", "```md\n## ![code](a.png)\n```", "## ![missing][unknown]"] {
            assert!(runs(text).is_empty(), "{text}");
        }
        for literal in ["# right", "## right", "> right", "1. right", "- right", "---", "    indented", "\tindented"] {
            for heading in [format!("## ![x](a.png) {literal}"), format!("## {literal} ![x](a.png)")] {
                let prose: String = runs(&heading).into_iter().filter_map(|run| match run { MarkdownMediaRun::Prose(text) => Some(text), _ => None }).collect();
                let root = markdown::to_mdast(&prose, &markdown::ParseOptions::gfm()).unwrap();
                assert!(root.children().unwrap().iter().all(|node| matches!(node, markdown::mdast::Node::Paragraph(_))), "{heading}: {root:?}");
                assert_eq!(crate::assistant_citations::markdown_text(&prose).trim(), literal.trim(), "{heading}: {prose:?}; {root:?}");
            }
        }
        fn destinations(node: &markdown::mdast::Node, out: &mut Vec<(String, Option<String>)>) {
            if let markdown::mdast::Node::Link(link) = node { out.push((link.url.clone(), link.title.clone())); }
            if let Some(children) = node.children() { for child in children { destinations(child, out); } }
        }
        for link in ["<https://example.com/a?q=1&b=2#part>", "https://example.com/a", "www.example.com", "<team@example.com>", "team@example.com", "[**guide**](https://example.com/a \"title\")"] {
            for text in [format!("## {link} ![x](a.png)"), format!("## ![x](a.png) {link}")] {
                let original = markdown::to_mdast(&text, &markdown::ParseOptions::gfm()).unwrap();
                let prose: String = runs(&text).into_iter().filter_map(|run| match run { MarkdownMediaRun::Prose(text) => Some(text), _ => None }).collect();
                let reconstructed = markdown::to_mdast(&prose, &markdown::ParseOptions::gfm()).unwrap();
                let (mut expected, mut actual) = (Vec::new(), Vec::new());
                destinations(&original, &mut expected);
                destinations(&reconstructed, &mut actual);
                assert!(!expected.is_empty(), "{text}");
                assert_eq!(actual, expected, "{text}: {prose}");
                assert_eq!(crate::assistant_citations::normalize(&crate::assistant_citations::markdown_text(&prose)).trim(), crate::assistant_citations::normalize(&crate::assistant_citations::markdown_text(&text)).trim(), "{text}");
            }
        }
    }

    #[test]
    fn heading_streams_reuse_completed_blocks_and_remount_after_late_definitions() {
        let text = "# First ![x](a.png)\n\n## Tail ![y](b.png)";
        let document = MarkdownMediaDocument::new(text.into()).prepare();
        let next = document.updated(format!("{text} more").into());
        assert_eq!(document.append_delta(&next), Some(" more"));
        assert!(std::sync::Arc::ptr_eq(&document.index().paragraphs[&0], &next.index().paragraphs[&0]));
        assert_eq!(next.index().parsed_from, text.find("## Tail").unwrap());
        let text = "## Left ![x][p] right\n\nTail";
        let missing = MarkdownMediaDocument::new(text.into()).prepare();
        let resolved = missing.updated(format!("{text}\n\n[p]: a.png").into());
        assert!(missing.append_delta(&resolved).is_none());
        assert!(matches!(&resolved.index().paragraphs[&0][1], MarkdownMediaRun::Image { url, .. } if url == "a.png"));
        for text in ["## **left ![x][p] right**\n\n[p]: a.png\n\nTail", "Left ![x](a.png)\n===\n\nTail", "> ## ![x][p]\n\n[p]: a.png"] {
            let mut previous = MarkdownMediaDocument::new("".into()).prepare();
            for end in text.char_indices().map(|(offset, ch)| offset + ch.len_utf8()) {
                let next = previous.updated(text[..end].into());
                let full = MarkdownMediaDocument::new(text[..end].into()).prepare();
                assert_eq!(next.index().paragraphs, full.index().paragraphs, "{end}: {text}");
                previous = next;
            }
        }
        let text = "Prefix\n\n### ![x](a.png)";
        let fragment = "### ![x](a.png)";
        let root = markdown::to_mdast(fragment, &markdown::ParseOptions::gfm()).unwrap();
        assert_eq!(MarkdownMediaDocument::new(text.into()).runs_at(&root.children().unwrap()[0], text.find(fragment).unwrap()), runs(fragment));
    }

    #[test]
    fn linked_reference_images_keep_the_anchor_and_first_prose_definition() {
        let result = runs("[**left ![chart][pic] right**][guide]\n\n[pic]: local.png\n[guide]: https://example.com/first\n[guide]: javascript:bad");
        assert_eq!(result, vec![
            MarkdownMediaRun::Prose("[**left**][guide] \n\n[guide]: https://example.com/first".into()),
            MarkdownMediaRun::Image { url: "local.png".into(), alt: "chart".into(), link: Some("https://example.com/first".into()) },
            MarkdownMediaRun::Prose(" [**right**][guide]\n\n[guide]: https://example.com/first".into()),
        ]);
        for label in ["[a &amp; b]", "[a &amp; b][]", "[a &amp; b][a &amp; b]"] {
            let text = format!("![x](a.png) {label}\n\n[a &amp; b]: https://host");
            let html: String = runs(&text).into_iter().filter_map(|run| match run {
                MarkdownMediaRun::Prose(text) => Some(markdown::to_html_with_options(&text, &markdown::Options::gfm()).unwrap()),
                _ => None,
            }).collect();
            assert_eq!(html, "<p><a href=\"https://host\">a &amp; b</a></p>\n", "{label}");
        }
        fn link_targets(text: &str) -> Vec<(String, Option<String>)> {
            use markdown::mdast::Node;
            fn collect<'a>(node: &'a Node, nodes: &mut Vec<&'a Node>) {
                nodes.push(node);
                if let Some(children) = node.children() { for child in children { collect(child, nodes); } }
            }
            let root = markdown::to_mdast(text, &markdown::ParseOptions::gfm()).unwrap();
            let mut nodes = Vec::new();
            collect(&root, &mut nodes);
            nodes.iter().filter_map(|node| match node {
                Node::Link(link) => Some((link.url.clone(), link.title.clone())),
                Node::LinkReference(link) => nodes.iter().find_map(|node| match node {
                    Node::Definition(definition) if definition.identifier == link.identifier => Some((definition.url.clone(), definition.title.clone())),
                    _ => None,
                }),
                _ => None,
            }).collect()
        }
        for (url, title, decoded_url, decoded_title) in [
            ("https://host/?q=&amp;amp;", "&amp;copy; and &amp;quot;", "https://host/?q=&amp;", "&copy; and &quot;"),
            ("https://host/?q=&#x26;copy;", "&#x26;#34; &amp;#x3c;", "https://host/?q=&copy;", "&#34; &#x3c;"),
            (r#"https://host/?a=1&b=2&amp;c=3\>d\<e\\f"#, r#"ordinary & query \"quote\" \\backslash"#, r#"https://host/?a=1&b=2&c=3>d<e\f"#, r#"ordinary & query "quote" \backslash"#),
        ] {
            for link in ["[a]", "[a][]", "[shown][a]", "[shown]\n[a]"] {
                for definition in [format!("[a]: <{url}> \"{title}\""), format!("[a]:\n  <{url}>\n  \"{title}\"")] {
                    let text = format!("![x](a.png) {link}\n\n{definition}");
                    let expected = vec![(decoded_url.to_owned(), Some(decoded_title.to_owned()))];
                    assert_eq!(link_targets(&text), expected, "original {text}");
                    let actual: Vec<_> = runs(&text).into_iter().filter_map(|run| match run {
                        MarkdownMediaRun::Prose(text) => Some(link_targets(&text)),
                        _ => None,
                    }).flatten().collect();
                    assert_eq!(actual, expected, "reconstructed {text}");
                }
            }
            let text = format!("![x](a.png) [a](\n<{url}>\n\"{title}\")");
            let expected = vec![(decoded_url.to_owned(), Some(decoded_title.to_owned()))];
            assert_eq!(link_targets(&text), expected, "original {text}");
            let actual: Vec<_> = runs(&text).into_iter().filter_map(|run| match run {
                MarkdownMediaRun::Prose(text) => Some(link_targets(&text)),
                _ => None,
            }).flatten().collect();
            assert_eq!(actual, expected, "reconstructed {text}");
        }
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
        assert_eq!(MarkdownMediaDocument::new(text.into()).runs_at(paragraph, 0), vec![
            MarkdownMediaRun::Prose("Before **bold** ".into()),
            MarkdownMediaRun::Image { url: "img/a.png".into(), alt: "chart".into(), link: None },
            MarkdownMediaRun::Prose(" and ".into()),
            MarkdownMediaRun::Image { url: "b.svg".into(), alt: "two".into(), link: None },
            MarkdownMediaRun::Prose(" after.".into()),
        ]);
        let empty = runs("![x](a.png) [](https://example.com)");
        assert!(matches!(&empty[0], MarkdownMediaRun::Image { url, .. } if url == "a.png"));
        let html: String = empty.into_iter().filter_map(|run| match run {
            MarkdownMediaRun::Prose(text) => Some(markdown::to_html_with_options(&text, &markdown::Options::gfm()).unwrap()),
            _ => None,
        }).collect();
        assert_eq!(html.trim_end(), "<p><a href=\"https://example.com\"></a></p>");
        let document = MarkdownMediaDocument::new("See [guide][g] ![chart](chart.png)\n\n[g]: https://example.com".into());
        let fragment = "See [guide][g] ";
        assert!(document.prose(fragment, &["g".into()]).contains("[g]: https://example.com"));
        assert!(document.prose(fragment, &["g".into()]).starts_with(fragment));
        assert_eq!(resolve_asset_url("http://host:1/base", "/api/assets/a"), Some("http://host:1/api/assets/a".into()));
        assert_eq!(resolve_asset_url("http://host:1", "https://local:2/api/assets/a"), Some("https://local:2/api/assets/a".into()));
        assert_eq!(resolve_asset_url("http://host:1", "javascript:bad"), None);
    }
}
