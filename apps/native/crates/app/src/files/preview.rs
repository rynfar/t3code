//! The file preview (`FilePreviewPanel.tsx`'s content column): `projects.readFile` on the network
//! runtime, line layout and syntax parsing on the background executor, then source with line
//! numbers (`ReadOnlySourcePreview.tsx`), rendered markdown (`FileMarkdownPreview.tsx`), an image
//! (`WorkspaceImagePreview`), a folder, or the read error. Recent reads are kept, a few files deep.

use std::collections::VecDeque;
use std::rc::Rc;

use gpui::{AnyElement, AppContext, Context, Entity, IntoElement, ParentElement, Render, ScrollHandle, Styled, Task, Window, div, prelude::*, px};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextViewState;
use gpui_component::{ActiveTheme, Rope, h_flex, v_flex};
use t3_client::session::RpcError;
use t3_core::files::{
    FileContents, FileFailure, ImageKind, LineIndex, READ_FILE, ReadFailure, decode_read_file, group_thousands, image_kind, is_host_path,
    is_markdown, mask_frontmatter, read_file_request, syntax_language,
};

use super::Root;
use super::code::CodeView;
use super::image::{self, ImageState};
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// Text files kept after leaving them, so tabbing back shows them at once.
const CACHE_LIMIT: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: Root,
    pub path: String,
    pub reveal_line: Option<u64>,
    pub reveal_request_id: u64,
}

/// A file's identity: the server, the workspace and the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    environment: String,
    cwd: String,
    path: String,
}

impl Key {
    fn of(target: &Target) -> Key {
        Key { environment: target.root.environment.clone(), cwd: target.root.workspace.cwd.clone(), path: target.path.clone() }
    }
}

pub struct TextFile {
    pub(super) key: Key,
    pub contents: String,
    pub lines: LineIndex,
    pub byte_length: u64,
    pub truncated: bool,
    pub highlighter: Option<SyntaxHighlighter>,
}

fn build_text_file(key: Key, file: FileContents) -> TextFile {
    let lines = LineIndex::new(&file.contents);
    let highlighter = syntax_language(&key.path).map(|language| {
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &Rope::from_str(&file.contents), None);
        highlighter
    });
    TextFile { key, contents: file.contents, lines, byte_length: file.byte_length, truncated: file.truncated, highlighter }
}

enum Content {
    Empty,
    Loading,
    Text(Rc<TextFile>),
    /// The path names a folder: the explorer shows it instead.
    Folder,
    Failed(ReadFailure),
    Image(ImageState),
}

pub struct Preview {
    state: Entity<AppState>,
    target: Option<Target>,
    key: Option<Key>,
    content: Content,
    cache: VecDeque<Rc<TextFile>>,
    generation: u64,
    _fetch: Option<Task<()>>,
    pub(super) code: CodeView,
    render_markdown: bool,
    markdown: Option<(Key, Entity<TextViewState>)>,
    markdown_scroll: ScrollHandle,
    mutation: Option<String>,
    media_scope: Option<std::sync::Arc<std::sync::Mutex<crate::media::MediaScope>>>,
    media_document: Option<crate::transcript::MediaDocument>,
    media_extensions: Option<gpui_component::text::MarkdownExtensions>,
    viewer: Entity<crate::media::MediaViewer>,
    handled_reveal: Option<(String, u64)>,
    /// A reveal shows the source; the rendered view returns once the user asks for it.
    rendered_after_reveal: Option<(String, u64)>,
}

impl Preview {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        crate::media::cache(&state, cx);
        let viewer = cx.new(crate::media::MediaViewer::new);
        Preview {
            state,
            target: None,
            key: None,
            content: Content::Empty,
            cache: VecDeque::new(),
            generation: 0,
            _fetch: None,
            code: CodeView::new(),
            render_markdown: false,
            markdown: None,
            markdown_scroll: ScrollHandle::new(),
            mutation: None,
            media_scope: None,
            media_document: None,
            media_extensions: None,
            viewer,
            handled_reveal: None,
            rendered_after_reveal: None,
        }
    }

    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if target == self.target {
            return;
        }
        let key = target.as_ref().map(Key::of);
        self.target = target;
        if key != self.key {
            self.key = key.clone();
            self.generation += 1;
            self._fetch = None;
            let cached = key.as_ref().and_then(|key| self.cache.iter().position(|file| file.key == *key));
            match (key, cached) {
                (None, _) => self.replace_content(Content::Empty, cx),
                (Some(_), Some(index)) => {
                    if let Some(file) = self.cache.remove(index) {
                        self.cache.push_back(file.clone());
                        self.show_text(file, cx);
                    }
                    // A cached file may be stale; read it again behind the cached view.
                    self.fetch(cx);
                }
                (Some(_), None) => {
                    self.replace_content(Content::Loading, cx);
                    self.fetch(cx);
                }
            }
        }
        self.apply_reveal();
        cx.notify();
    }

    fn replace_content(&mut self, content: Content, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.content, content);
        if let Content::Image(image) = previous {
            image.release(cx);
        }
        if !matches!(self.content, Content::Text(_)) {
            self.code.set_file(None);
        }
    }

    fn show_text(&mut self, file: Rc<TextFile>, cx: &mut Context<Self>) {
        if let Content::Text(current) = &self.content
            && current.key == file.key
            && current.contents == file.contents
        {
            return;
        }
        self.code.set_file(Some(file.clone()));
        if self.markdown.as_ref().is_some_and(|(key, _)| *key == file.key) {
            self.markdown = None;
        }
        self.replace_content(Content::Text(file), cx);
        self.apply_reveal();
    }

    /// Reads the target again (Refresh, a workspace mutation), keeping what shows until it lands.
    pub fn refetch(&mut self, cx: &mut Context<Self>) {
        if self.key.is_some() && !matches!(self.content, Content::Folder) {
            self.fetch(cx);
        }
    }

    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        if matches!(self.content, Content::Failed(_) | Content::Loading) || matches!(&self.content, Content::Image(image) if image.failed()) {
            self.fetch(cx);
        }
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        let key = Key::of(&target);
        #[cfg(feature = "snapshot")]
        if let Some(fixture) = cx.try_global::<MarkdownFixture>().filter(|fixture| fixture.path == key.path).cloned() {
            let file = FileContents { relative_path: key.path.clone(), contents: fixture.text, byte_length: 0, truncated: false };
            self.render_markdown = true;
            self.replace_content(Content::Text(Rc::new(build_text_file(key, file))), cx);
            cx.notify();
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        match image_kind(&key.path) {
            Some(kind) => {
                if !matches!(self.content, Content::Image(_)) {
                    self.replace_content(Content::Image(ImageState::Loading), cx);
                }
                let state = self.state.read(cx);
                let task = image::load(state, &target.root, &key.path, kind, self.mutation.as_deref());
                self._fetch = Some(cx.spawn(async move |this, cx| {
                    let result = match task {
                        Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                        None => Err("Not connected to the server.".to_owned()),
                    };
                    let decoded = match result {
                        Ok(bytes) => cx.background_executor().spawn(async move { image::decode(bytes, kind == ImageKind::Svg) }).await,
                        Err(error) => Err(error),
                    };
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.replace_content(Content::Image(ImageState::from(decoded)), cx);
                            cx.notify();
                        }
                    });
                }));
            }
            None => {
                let state = self.state.read(cx);
                let Some(client) = state.client_handle() else {
                    if !matches!(self.content, Content::Text(_)) {
                        self.replace_content(Content::Failed(ReadFailure::message("Not connected to the server.")), cx);
                    }
                    return;
                };
                let payload = read_file_request(&key.cwd, &key.path);
                let request = state.runtime().spawn(async move {
                    match client.request(READ_FILE, payload).await {
                        Ok(value) => decode_read_file(value).map_err(ReadFailure::message),
                        Err(RpcError::Failure(failure)) => Err(ReadFailure::from_error(&failure.error, &failure.message)),
                        Err(RpcError::Closed) => Err(ReadFailure::message("Not connected to the server.")),
                    }
                });
                self._fetch = Some(cx.spawn(async move |this, cx| {
                    let result = request.await.unwrap_or_else(|_| Err(ReadFailure::message("The file could not be read.")));
                    let result = match result {
                        Ok(file) => Ok(cx.background_executor().spawn(async move { build_text_file(key, file) }).await),
                        Err(failure) => Err(failure),
                    };
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.finish_read(result, cx);
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }

    fn finish_read(&mut self, result: Result<TextFile, ReadFailure>, cx: &mut Context<Self>) {
        match result {
            Ok(file) => {
                let file = Rc::new(file);
                self.cache.retain(|cached| cached.key != file.key);
                self.cache.push_back(file.clone());
                while self.cache.len() > CACHE_LIMIT {
                    self.cache.pop_front();
                }
                self.show_text(file, cx);
            }
            Err(failure) if failure.failure == Some(FileFailure::NotFile) => self.replace_content(Content::Folder, cx),
            // A failed refresh keeps the file it read before (`file.error && file.data === null`).
            Err(_) if matches!(self.content, Content::Text(_)) => {}
            Err(failure) => self.replace_content(Content::Failed(failure), cx),
        }
    }

    /// `useFileLineReveal`: once per reveal request, the line scrolls to the middle and tints.
    fn apply_reveal(&mut self) {
        let (Some(target), Content::Text(file)) = (&self.target, &self.content) else { return };
        let Some(line) = target.reveal_line else {
            self.code.set_reveal(None);
            return;
        };
        let request = (target.path.clone(), target.reveal_request_id);
        if self.handled_reveal.as_ref() == Some(&request) {
            return;
        }
        self.handled_reveal = Some(request);
        self.code.reveal(file.lines.reveal_index(line));
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.content, Content::Folder)
    }

    pub fn shows_text(&self) -> bool {
        matches!(self.content, Content::Text(_)) && !self.renders_markdown()
    }

    fn renders_markdown(&self) -> bool {
        let Some(target) = &self.target else { return false };
        let revealing = target.reveal_line.is_some()
            && self.rendered_after_reveal.as_ref() != Some(&(target.path.clone(), target.reveal_request_id));
        self.render_markdown && is_markdown(&target.path) && !revealing && matches!(self.content, Content::Text(_))
    }

    pub fn set_render_markdown(&mut self, render: bool, cx: &mut Context<Self>) {
        if render && let Some(target) = &self.target {
            self.rendered_after_reveal = Some((target.path.clone(), target.reveal_request_id));
        }
        if render != self.render_markdown {
            self.render_markdown = render;
            cx.notify();
        }
    }

    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.code.set_wrap(wrap);
        cx.notify();
    }

    /// The truncation notice the web shows over drafts, markdown and HTML; native reads text
    /// only, so it shows over every truncated file.
    pub fn truncation_notice(&self) -> Option<String> {
        let Content::Text(file) = &self.content else { return None };
        file.truncated.then(|| format!("Preview limited to the first 1 MB of a {} byte file.", group_thousands(file.byte_length)))
    }

    pub fn set_mutation(&mut self, mutation: Option<String>, cx: &mut Context<Self>) {
        if self.mutation == mutation { return; }
        self.mutation = mutation;
        if let Some(scope) = &self.media_scope { scope.lock().unwrap().workspace_mutation_id = self.mutation.clone(); }
        cx.notify();
    }

    fn render_markdown(&mut self, file: &Rc<TextFile>, cx: &mut Context<Self>) -> AnyElement {
        let state = match &self.markdown {
            Some((key, state)) if *key == file.key => state.clone(),
            _ => {
                let text = mask_frontmatter(&file.contents).into_owned();
                if let Some(document) = &self.media_document { *document.write().unwrap() = std::sync::Arc::new(t3_core::media::MarkdownMediaDocument::new(std::sync::Arc::from(text.as_str()))); }
                else { self.media_document = Some(crate::transcript::media_document(std::sync::Arc::from(text.as_str()))); }
                let state = cx.new(|cx| {
                    #[cfg(feature = "snapshot")]
                    if !text.contains("![") { cx.observe_self(|_, cx| crate::snapshot::record_media_notification(cx)).detach(); }
                    TextViewState::markdown(&text, cx).selectable(true)
                });
                #[cfg(feature = "snapshot")]
                if !text.contains("![") { crate::snapshot::record_media_prose(state.entity_id(), &text, cx); }
                self.markdown = Some((file.key.clone(), state.clone()));
                state
            }
        };
        let target = self.target.as_ref().unwrap();
        let scope = crate::media::MediaScope {
            owner: cx.entity_id(),
            thread_id: Some(target.root.workspace.thread_id.clone()),
            workspace_root: Some(t3_core::media::markdown_image_base_dir(&target.root.workspace.cwd, &target.path)),
            workspace_mutation_id: self.mutation.clone(),
            viewer: self.viewer.downgrade(),
        };
        if let Some(current) = &self.media_scope { *current.lock().unwrap() = scope; }
        else {
            let scope = std::sync::Arc::new(std::sync::Mutex::new(scope));
            self.media_extensions = Some(crate::transcript::markdown_extensions(self.media_document.clone().unwrap(), Some(scope.clone())));
            self.media_scope = Some(scope);
        }
        let p = palette(cx);
        div()
            .id("file-markdown")
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                div().id("file-markdown-scroll").size_full().overflow_y_scroll().track_scroll(&self.markdown_scroll).child(
                    div()
                        .mx_auto()
                        .w_full()
                        .max_w(px(896.))
                        .px(px(24.))
                        .py(px(20.))
                        .text_size(px(14.))
                        .line_height(px(22.75))
                        .text_color(hsla(p.foreground).opacity(0.9))
                        .child(crate::transcript::markdown::markdown_view(&state, cx).markdown_extensions(self.media_extensions.clone().unwrap())),
                ),
            )
            .vertical_scrollbar(&self.markdown_scroll)
            .into_any_element()
    }

    fn render_failure(&self, failure: &ReadFailure, cx: &Context<Self>) -> AnyElement {
        let p = palette(cx);
        let target = self.target.as_ref();
        let host = target.is_some_and(|target| is_host_path(&target.path));
        let cwd = failure.cwd.clone().or_else(|| target.map(|target| target.root.workspace.cwd.clone())).unwrap_or_default();
        let mono = cx.theme().mono_font_family.clone();
        if failure.failure == Some(FileFailure::Binary) {
            let name = target.map(|target| t3_core::files::basename(&target.path).to_owned()).unwrap_or_default();
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(Icon::File.svg().size(px(20.)).text_color(hsla(p.muted_foreground)))
                .child(div().text_color(hsla(p.foreground)).child(name))
                .child(failure.describe())
                .into_any_element();
        }
        div()
            .id("file-error")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                v_flex()
                    .my_auto()
                    .flex_none()
                    .gap(px(12.))
                    .px(px(24.))
                    .py(px(24.))
                    .text_center()
                    .text_xs()
                    .line_height(px(19.5))
                    .child(div().text_color(hsla(p.destructive)).child(failure.describe()))
                    .children(failure.attempted_path.clone().map(|path| {
                        v_flex()
                            .text_color(hsla(p.muted_foreground))
                            .child("Attempted path")
                            .child(div().font_family(mono.clone()).text_color(hsla(p.foreground)).child(path))
                    }))
                    .when(!host, |this| {
                        this.child(
                            div()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!("Workspace folder: {cwd}. Check the link's path or locate the file in Files.")),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// A still skeleton of a few code lines while the file loads (design-system §6.8).
fn loading(cx: &Context<Preview>) -> AnyElement {
    let fill = hsla(palette(cx).muted_foreground).opacity(0.15);
    v_flex()
        .flex_1()
        .gap(px(10.))
        .px(px(16.))
        .py(px(12.))
        .children([180., 260., 220., 120., 300., 200.].map(|width| {
            h_flex().gap(px(16.)).child(div().w(px(16.)).h(px(10.)).rounded_full().bg(fill)).child(div().w(px(width)).h(px(10.)).rounded_full().bg(fill))
        }))
        .into_any_element()
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            Content::Empty | Content::Folder => div().into_any_element(),
            Content::Loading => loading(cx),
            Content::Failed(failure) => self.render_failure(&failure.clone(), cx),
            Content::Image(image) => image.render(cx),
            Content::Text(file) => {
                let file = file.clone();
                if self.renders_markdown() { self.render_markdown(&file, cx) } else { self.render_code(window, cx) }
            }
        };
        v_flex().relative().size_full().min_h_0().child(body).child(self.viewer.clone())
    }
}

#[cfg(feature = "snapshot")]
#[derive(Clone, serde::Deserialize)]
pub(crate) struct MarkdownFixture { pub path: String, pub text: String }
#[cfg(feature = "snapshot")]
impl gpui::Global for MarkdownFixture {}
