//! The file preview (`FilePreviewPanel.tsx`'s content column): `projects.readFile` on the network
//! runtime, line layout and syntax parsing on the background executor, then in-place editing
//! (`editor.rs`), source with line numbers (`code.rs`), rendered markdown, standalone Mermaid
//! diagrams (`mermaid.rs`), delimited CSV/TSV virtual tables (`table.rs`), Jupyter notebooks
//! (`notebook.rs`), binary/hex & archive inspection (`binary.rs`), images (`image.rs`), audio
//! (`audio.rs`), fonts (`font.rs`), video, folders, or read errors.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, ScrollHandle, Styled, Task, Window, div, prelude::*, px,
};
use gpui_component::highlighter::SyntaxHighlighter;
use gpui_component::scroll::ScrollableElement;
use gpui_component::text::TextViewState;
use gpui_component::{ActiveTheme, Rope, h_flex, v_flex};
use t3_client::session::RpcError;
use t3_core::files::pdf::PdfPositionStore;
use t3_core::files::save::FileSaveTarget;
use t3_core::files::{
    FileContents, FileFailure, ImageKind, LineIndex, READ_FILE, ReadFailure, binary_container_kind,
    decode_read_file, delimited_separator, group_thousands, image_kind, is_audio, is_font, is_host_path,
    is_markdown, is_mermaid, is_notebook, is_pdf, mask_frontmatter, office_kind, read_file_request,
    syntax_language,
};

use super::Root;
use super::audio::{self, AudioState};
use super::binary::{self, BinaryState, BinaryViewMode};
use super::attachment::{AttachmentTarget, DecodedAttachment, Kind as AttachmentKind};
use super::code::CodeView;
use super::editor::{FileEditorController, target_to_save_target};
use super::font::{self, FontState};
use super::image::{self, ImageState};
use super::mermaid::MermaidView;
use super::notebook::NotebookView;
use super::office::{self, OfficeReady, OfficeState};
use super::pdf::{self, PdfReady, PdfState};
use super::table::TableView;
use crate::app_state::AppState;
use crate::theme::{hsla, palette};

/// Text files kept after leaving them, so tabbing back shows them at once.
const CACHE_LIMIT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderedMode {
    Markdown,
    Mermaid,
    Table,
    Notebook,
    Html,
}

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
    pub(super) environment: String,
    pub(super) cwd: String,
    pub(super) path: String,
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

pub(super) fn build_text_file(key: Key, file: FileContents) -> TextFile {
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
    Video {
        source: t3_core::media::MediaSource,
        player: Entity<crate::media::video::VideoPlayer>,
    },
    Audio(AudioState),
    Font(FontState),
    Pdf(PdfState),
    Office(OfficeState),
    Binary(BinaryState),
}

pub struct Preview {
    pub(super) state: Entity<AppState>,
    pub(super) target: Option<Target>,
    attachment: Option<AttachmentTarget>,
    key: Option<Key>,
    content: Content,
    cache: VecDeque<Rc<TextFile>>,
    generation: u64,
    _fetch: Option<Task<()>>,
    pub(super) code: CodeView,
    pub(super) editor: FileEditorController,
    pub(super) table: TableView,
    pub(super) notebook: NotebookView,
    pub(super) mermaid: MermaidView,
    pub(super) pdf_positions: PdfPositionStore,
    pub(super) content_focus: FocusHandle,
    pending_content_focus: bool,
    render_markdown: bool,
    render_mermaid: bool,
    render_table: bool,
    render_notebook: bool,
    render_html: bool,
    html: Option<(Rc<TextFile>, Rc<crate::webview::WebView>)>,
    markdown: Option<(Key, Entity<TextViewState>)>,
    markdown_scroll: ScrollHandle,
    mutation: Option<String>,
    media_scope: Option<std::sync::Arc<std::sync::Mutex<crate::media::MediaScope>>>,
    media_document: Option<crate::transcript::MediaDocument>,
    media_extensions: Option<gpui_component::text::MarkdownExtensions>,
    pub(super) viewer: Entity<crate::media::MediaViewer>,
    handled_reveal: Option<(String, u64)>,
    /// A reveal shows the source; the rendered view returns once the user asks for it.
    rendered_after_reveal: Option<(String, u64)>,
    _activation: Option<gpui::Subscription>,
}

impl Preview {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        crate::media::cache(&state, cx);
        let viewer = cx.new(crate::media::MediaViewer::new);
        let mut preview = Preview {
            state,
            target: None,
            attachment: None,
            key: None,
            content: Content::Empty,
            cache: VecDeque::new(),
            generation: 0,
            _fetch: None,
            code: CodeView::new(),
            editor: FileEditorController::new(),
            table: TableView::new(),
            notebook: NotebookView::new(),
            mermaid: MermaidView::new(),
            pdf_positions: PdfPositionStore::default(),
            content_focus: cx.focus_handle().tab_stop(true),
            pending_content_focus: false,
            render_markdown: false,
            render_mermaid: true,
            render_table: true,
            render_notebook: true,
            render_html: false,
            html: None,
            markdown: None,
            markdown_scroll: ScrollHandle::new(),
            mutation: None,
            media_scope: None,
            media_document: None,
            media_extensions: None,
            viewer,
            handled_reveal: None,
            rendered_after_reveal: None,
            _activation: None,
        };
        cx.on_release(|this: &mut Preview, cx| {
            let prev = std::mem::replace(&mut this.content, Content::Empty);
            match prev {
                Content::Image(image) => image.release(cx),
                Content::Audio(mut audio) => audio.stop(),
                Content::Pdf(mut pdf) => pdf.release(cx),
                _ => {}
            }
        })
        .detach();
        preview.refresh_write_scope(cx);
        preview
    }

    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        self.set_attachment(None, cx);
        self.refresh_write_scope(cx);
        if target == self.target {
            return;
        }
        let key = target.as_ref().map(Key::of);
        let key_changed = key != self.key;
        if key_changed {
            self.release_active_editor_on_target_change(cx);
        }
        self.target = target;
        if key_changed {
            self.key = key.clone();
            self.generation += 1;
            self._fetch = None;
            crate::media::video::retire_owner(cx.entity_id(), cx);
            self.viewer.update(cx, |viewer, cx| viewer.dismiss(cx));
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

    pub(super) fn set_attachment(&mut self, attachment: Option<AttachmentTarget>, cx: &mut Context<Self>) {
        if self.attachment == attachment {
            return;
        }
        if self.target.is_some() {
            self.set_target(None, cx);
        }
        self.attachment = attachment;
        self._fetch = None;
        self.generation += 1;
        self.key = None;
        self.viewer.update(cx, |viewer, cx| viewer.dismiss(cx));
        self.replace_content(Content::Empty, cx);
        self.fetch_attachment(cx);
        cx.notify();
    }

    fn fetch_attachment(&mut self, cx: &mut Context<Self>) {
        let Some(attachment) = self.attachment.clone() else { return };
        let Some(resource) = attachment.resource() else {
            self.replace_content(Content::Failed(ReadFailure::message("No preview for this file")), cx);
            return;
        };
        let app = self.state.read(cx);
        let Some(client) = app.client_handle().filter(|_| app.environment() == Some(&attachment.environment)) else {
            self.replace_content(Content::Failed(ReadFailure::message("Not connected to the server.")), cx);
            return;
        };
        let kind = AttachmentKind::of(&attachment.file);
        let limit = kind.limit();
        if attachment.file.size_bytes > limit as u64 {
            self.replace_content(Content::Failed(ReadFailure::message("This file exceeds the preview size limit.")), cx);
            return;
        }
        let request = app.runtime().spawn(async move {
            let asset = t3_client::assets::mint_media_url(&client, &resource).await?;
            client.asset_bytes(&asset.relative_url, limit).await
                .map_err(|_| "Unable to load attachment.".to_owned())
        });
        self.generation += 1;
        let generation = self.generation;
        self.replace_content(Content::Loading, cx);
        let doc_key = format!("{}:attachment:{}", attachment.environment, attachment.file.id);
        let key = Key {
            environment: attachment.environment,
            cwd: format!("attachment:{}", attachment.file.id),
            path: attachment.file.name,
        };
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let decoded = match request.await {
                Ok(Ok(bytes)) => cx.background_executor().spawn(async move { kind.decode(key, bytes) }).await,
                Ok(Err(error)) => Err(error),
                Err(_) => Err("Unable to load attachment.".to_owned()),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let content = match decoded {
                    Ok(DecodedAttachment::Text(file)) => {
                        this.show_text(Rc::new(*file), cx);
                        cx.notify();
                        return;
                    }
                    Ok(DecodedAttachment::Audio(audio)) => Content::Audio(AudioState::from_decoded(Ok(audio))),
                    Ok(DecodedAttachment::Binary(binary)) => Content::Binary(BinaryState::from(Ok(binary))),
                    Ok(DecodedAttachment::Font(font)) => Content::Font(FontState::from_decoded(Ok(font), cx)),
                    Ok(DecodedAttachment::Image(image)) => Content::Image(ImageState::from(Ok(image))),
                    Ok(DecodedAttachment::Office(office)) => Content::Office(OfficeState::from_decoded(Ok(office))),
                    Ok(DecodedAttachment::Pdf(pdf)) => {
                        Content::Pdf(PdfState::from_decoded(doc_key, Ok(pdf), &this.pdf_positions))
                    }
                    Err(error) => Content::Failed(ReadFailure::message(error)),
                };
                this.replace_content(content, cx);
                cx.notify();
            });
        }));
    }

    pub(super) fn attachment_resource(&self) -> Option<t3_core::media::AssetResource> {
        self.attachment.as_ref()?.resource()
    }

    pub(super) fn retry_attachment(&mut self, cx: &mut Context<Self>) {
        if matches!(self.content, Content::Failed(_)) {
            self.fetch_attachment(cx);
            cx.notify();
        }
    }

    fn replace_content(&mut self, content: Content, cx: &mut Context<Self>) {
        self.html = None;
        let previous = std::mem::replace(&mut self.content, content);
        match previous {
            Content::Image(image) => image.release(cx),
            Content::Audio(mut audio) => audio.stop(),
            Content::Pdf(mut pdf) => {
                pdf.save_position(&mut self.pdf_positions);
                pdf.release(cx);
            }
            _ => {}
        }
        if !matches!(self.content, Content::Text(_)) {
            self.code.set_file(None);
            self.table.clear();
            self.notebook.release_images(cx);
            self.mermaid.release(cx);
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

    pub(super) fn current_text_contents(&self) -> Option<String> {
        match &self.content {
            Content::Text(file) => Some(file.contents.clone()),
            _ => None,
        }
    }

    pub(super) fn apply_saved_text_locally(
        &mut self,
        target: &FileSaveTarget,
        contents: &str,
        cx: &mut Context<Self>,
    ) {
        let key = Key {
            environment: target.environment_id.clone(),
            cwd: target.cwd.clone(),
            path: target.relative_path.clone(),
        };
        let file = Rc::new(build_text_file(
            key.clone(),
            FileContents {
                relative_path: target.relative_path.clone(),
                contents: contents.to_owned(),
                byte_length: contents.len() as u64,
                truncated: false,
            },
        ));
        self.cache.retain(|cached| cached.key != key);
        self.cache.push_back(file.clone());
        while self.cache.len() > CACHE_LIMIT {
            self.cache.pop_front();
        }
        if self.key.as_ref() == Some(&key) {
            self.show_text(file, cx);
        }
    }

    /// Reads the target again (Refresh, a workspace mutation), keeping what shows until it lands.
    pub fn refetch(&mut self, cx: &mut Context<Self>) {
        if self.key.is_some() && !matches!(self.content, Content::Folder) && !self.editor.is_dirty() {
            self.fetch(cx);
        }
    }

    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        self.refresh_write_scope(cx);
        let needs_retry = matches!(self.content, Content::Failed(_) | Content::Loading)
            || matches!(&self.content, Content::Image(image) if image.failed())
            || matches!(&self.content, Content::Audio(audio) if audio.failed())
            || matches!(&self.content, Content::Font(font) if font.failed())
            || matches!(&self.content, Content::Pdf(pdf) if pdf.failed())
            || matches!(&self.content, Content::Office(office) if office.failed())
            || matches!(&self.content, Content::Binary(binary) if binary.failed());
        if needs_retry {
            if self.attachment.is_some() {
                self.fetch_attachment(cx);
            } else {
                self.fetch(cx);
            }
        }
    }

    fn fetch_binary(&mut self, target: Target, generation: u64, cx: &mut Context<Self>) {
        if !matches!(self.content, Content::Binary(_)) {
            self.replace_content(Content::Binary(BinaryState::Loading), cx);
        }
        let state = self.state.read(cx);
        let path = target.path.clone();
        let task = binary::load(state, &target.root, &path, self.mutation.as_deref());
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = match task {
                Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                None => Err("Not connected to the server.".to_owned()),
            };
            let decoded = match result {
                Ok(bytes) => {
                    let decode_path = path.clone();
                    Ok(cx.background_executor().spawn(async move { binary::decode(&decode_path, bytes) }).await)
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.replace_content(Content::Binary(BinaryState::from(decoded)), cx);
                    cx.notify();
                }
            });
        }));
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

        if t3_core::media::is_workspace_video_preview_path(&key.path) {
            self._fetch = None;
            let source = t3_core::media::MediaSource::Asset(t3_core::media::AssetResource::MediaFile {
                thread_id: target.root.workspace.thread_id.clone(),
                path: key.path.clone(),
            });
            let scope = crate::media::MediaScope {
                owner: cx.entity_id(),
                thread_id: Some(target.root.workspace.thread_id.clone()),
                workspace_root: Some(target.root.workspace.cwd.clone()),
                workspace_mutation_id: self.mutation.clone(),
                source_control_media: false,
                source_control_media_repository: None,
                viewer: self.viewer.downgrade(),
            };
            let viewer_handle = self.viewer.downgrade();
            let viewer_item = crate::media::ViewerItem::video(source.clone(), key.path.clone());
            let expand_scope = scope.clone();
            let options = crate::media::video::VideoPlayerOptions {
                source: source.clone(),
                name: key.path.clone().into(),
                revision: self.mutation.clone(),
                hints: t3_core::media::MediaHints { width: None, height: None, explicit_video: true },
                modal: false,
                max_width: 820.,
                max_height: 540.,
                on_expand: Some(std::sync::Arc::new(move |window, cx| {
                    let _ = viewer_handle.update(cx, |viewer, cx| viewer.open_with_scope(vec![viewer_item.clone()], 0, Some(expand_scope.clone()), window, cx));
                })),
                on_open_external: None,
                on_close_modal: None,
            };
            if let Content::Video { source: existing, player } = &mut self.content
                && *existing == source
            {
                player.update(cx, |player, cx| player.update_options(options, Some(scope), cx));
                cx.notify();
                return;
            }
            let player = cx.new(|cx| crate::media::video::VideoPlayer::new(options, Some(scope), cx));
            self.replace_content(Content::Video { source, player }, cx);
            cx.notify();
            return;
        }

        if is_audio(&key.path) {
            if !matches!(self.content, Content::Audio(_)) {
                self.replace_content(Content::Audio(AudioState::Loading), cx);
            }
            let state = self.state.read(cx);
            let task = audio::load(state, &target.root, &key.path, self.mutation.as_deref());
            self._fetch = Some(cx.spawn(async move |this, cx| {
                let result = match task {
                    Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                    None => Err("Not connected to the server.".to_owned()),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.replace_content(Content::Audio(AudioState::from_decoded(result)), cx);
                        cx.notify();
                    }
                });
            }));
            return;
        }

        if is_font(&key.path) {
            if !matches!(self.content, Content::Font(_)) {
                self.replace_content(Content::Font(FontState::Loading), cx);
            }
            let state = self.state.read(cx);
            let task = font::load(state, &target.root, &key.path, self.mutation.as_deref());
            self._fetch = Some(cx.spawn(async move |this, cx| {
                let result = match task {
                    Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                    None => Err("Not connected to the server.".to_owned()),
                };
                let decoded = match result {
                    Ok(bytes) => cx.background_executor().spawn(async move { font::decode(bytes) }).await,
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        let font_state = FontState::from_decoded(decoded, cx);
                        this.replace_content(Content::Font(font_state), cx);
                        cx.notify();
                    }
                });
            }));
            return;
        }

        if let Some(kind) = image_kind(&key.path) {
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
            return;
        }

        if is_pdf(&key.path) {
            if !matches!(self.content, Content::Pdf(_)) {
                self.replace_content(Content::Pdf(PdfState::Loading), cx);
            }
            let state = self.state.read(cx);
            let doc_key = format!("{}:{}:{}", key.environment, key.cwd, key.path);
            let task = pdf::load(state, &target.root, &key.path, self.mutation.as_deref());
            self._fetch = Some(cx.spawn(async move |this, cx| {
                let result = match task {
                    Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                    None => Err("Not connected to the server.".to_owned()),
                };
                let decoded = match result {
                    Ok(bytes) => cx.background_executor().spawn(async move { pdf::decode(bytes) }).await,
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        let pdf_state = PdfState::from_decoded(doc_key, decoded, &this.pdf_positions);
                        this.replace_content(Content::Pdf(pdf_state), cx);
                        cx.notify();
                    }
                });
            }));
            return;
        }

        if let Some(kind) = office_kind(&key.path) {
            if !matches!(self.content, Content::Office(_)) {
                self.replace_content(Content::Office(OfficeState::Loading), cx);
            }
            let state = self.state.read(cx);
            let task = office::load(state, &target.root, &key.path, self.mutation.as_deref());
            self._fetch = Some(cx.spawn(async move |this, cx| {
                let result = match task {
                    Some(task) => task.await.unwrap_or_else(|error| Err(error.to_string())),
                    None => Err("Not connected to the server.".to_owned()),
                };
                let decoded = match result {
                    Ok(bytes) => cx.background_executor().spawn(async move { office::decode(kind, bytes) }).await,
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.replace_content(Content::Office(OfficeState::from_decoded(decoded)), cx);
                        cx.notify();
                    }
                });
            }));
            return;
        }

        if binary_container_kind(&key.path).is_some() {
            self.fetch_binary(target, generation, cx);
            return;
        }

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
                    this.finish_read(result, generation, cx);
                    cx.notify();
                }
            });
        }));
    }

    fn finish_read(&mut self, result: Result<TextFile, ReadFailure>, generation: u64, cx: &mut Context<Self>) {
        match result {
            Ok(mut file) => {
                if let Some(target) = &self.target {
                    let save_target = target_to_save_target(target);
                    self.editor.set_server_text(file.contents.clone());
                    self.editor.clear_confirmed_if_matching(&save_target, &file.contents);
                    // Preserve unsaved optimistic draft across refreshes/tab switches.
                    if let Some(unsaved) = self.editor.optimistic_for(&save_target).map(str::to_owned)
                        && unsaved != file.contents
                    {
                        file = build_text_file(
                            file.key.clone(),
                            FileContents {
                                relative_path: target.path.clone(),
                                byte_length: unsaved.len() as u64,
                                truncated: false,
                                contents: unsaved,
                            },
                        );
                    }
                }
                let file = Rc::new(file);
                self.cache.retain(|cached| cached.key != file.key);
                self.cache.push_back(file.clone());
                while self.cache.len() > CACHE_LIMIT {
                    self.cache.pop_front();
                }
                self.show_text(file, cx);
            }
            Err(failure) if failure.failure == Some(FileFailure::NotFile) => self.replace_content(Content::Folder, cx),
            Err(failure) if failure.failure == Some(FileFailure::Binary) => {
                if let Some(target) = self.target.clone() {
                    self.fetch_binary(target, generation, cx);
                } else {
                    self.replace_content(Content::Failed(failure), cx);
                }
            }
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

    pub fn is_editing(&self) -> bool {
        self.editor.is_editing()
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.is_dirty()
    }

    pub fn can_edit_current(&self, _cx: &App) -> bool {
        let Some(target) = &self.target else { return false };
        if is_host_path(&target.path) {
            return false;
        }
        let Content::Text(file) = &self.content else { return false };
        !file.truncated
    }

    pub fn rendered_mode(&self) -> Option<RenderedMode> {
        let path = self.target.as_ref().map(|target| &target.path)
            .or_else(|| self.attachment.as_ref().map(|target| &target.file.name))?;
        if is_mermaid(path) {
            Some(RenderedMode::Mermaid)
        } else if is_markdown(path) {
            Some(RenderedMode::Markdown)
        } else if delimited_separator(path).is_some() {
            Some(RenderedMode::Table)
        } else if is_notebook(path) {
            Some(RenderedMode::Notebook)
        } else if cfg!(target_os = "macos") && (is_html(path) || self.attachment.as_ref().is_some_and(|target| {
            target.file.mime_type.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
        })) {
            Some(RenderedMode::Html)
        } else {
            None
        }
    }

    fn reveal_forces_source(&self) -> bool {
        let Some(target) = &self.target else { return false };
        target.reveal_line.is_some()
            && self.rendered_after_reveal.as_ref() != Some(&(target.path.clone(), target.reveal_request_id))
    }

    pub fn is_rendered_active(&self) -> bool {
        if self.editor.is_editing() || !matches!(self.content, Content::Text(_)) || self.reveal_forces_source() {
            return false;
        }
        match self.rendered_mode() {
            Some(RenderedMode::Markdown) => self.render_markdown,
            Some(RenderedMode::Mermaid) => self.render_mermaid,
            Some(RenderedMode::Table) => self.render_table,
            Some(RenderedMode::Notebook) => self.render_notebook,
            Some(RenderedMode::Html) => self.render_html,
            None => false,
        }
    }

    pub fn shows_text(&self) -> bool {
        matches!(self.content, Content::Text(_)) && !self.is_rendered_active()
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

    pub fn set_render_mermaid(&mut self, render: bool, cx: &mut Context<Self>) {
        if render && let Some(target) = &self.target {
            self.rendered_after_reveal = Some((target.path.clone(), target.reveal_request_id));
        }
        if render != self.render_mermaid {
            self.render_mermaid = render;
            cx.notify();
        }
    }

    pub fn set_render_table(&mut self, render: bool, cx: &mut Context<Self>) {
        if render && let Some(target) = &self.target {
            self.rendered_after_reveal = Some((target.path.clone(), target.reveal_request_id));
        }
        if render != self.render_table {
            self.render_table = render;
            cx.notify();
        }
    }

    pub fn set_render_notebook(&mut self, render: bool, cx: &mut Context<Self>) {
        if render && let Some(target) = &self.target {
            self.rendered_after_reveal = Some((target.path.clone(), target.reveal_request_id));
        }
        if render != self.render_notebook {
            self.render_notebook = render;
            cx.notify();
        }
    }

    pub fn set_render_html(&mut self, render: bool, cx: &mut Context<Self>) {
        let revealed = self.reveal_forces_source();
        if render && let Some(target) = &self.target {
            self.rendered_after_reveal = Some((target.path.clone(), target.reveal_request_id));
        }
        if self.render_html != render || (render && revealed) {
            self.render_html = render;
            if !render {
                self.html = None;
            }
            cx.notify();
        }
    }

    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.code.set_wrap(wrap);
        self.editor.set_wrap(wrap);
        cx.notify();
    }

    /// The truncation notice shown over truncated text previews.
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

    pub(super) fn binary_state(&self) -> Option<&BinaryState> {
        match &self.content {
            Content::Binary(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn binary_state_mut(&mut self) -> Option<&mut BinaryState> {
        match &mut self.content {
            Content::Binary(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn set_binary_mode(&mut self, mode: BinaryViewMode, cx: &mut Context<Self>) {
        if let Content::Binary(state) = &mut self.content {
            state.set_mode(mode);
            cx.notify();
        }
    }

    pub(super) fn sqlite_select_table(&mut self, table_name: String, cx: &mut Context<Self>) {
        let already_selected = matches!(
            &self.content,
            Content::Binary(BinaryState::Ready(ready))
                if matches!(
                    ready.sqlite.as_ref(),
                    Some(binary::SqliteData::Ready(viewer))
                        if viewer.selected_table.as_deref() == Some(table_name.as_str())
                            && viewer.page == 0
                            && matches!(viewer.page_state, binary::SqlitePageState::Ready(_))
                )
        );
        if already_selected {
            return;
        }
        self.sqlite_query_page(table_name, 0, cx);
    }

    pub(super) fn sqlite_select_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let table_name = match &self.content {
            Content::Binary(BinaryState::Ready(ready)) => match ready.sqlite.as_ref() {
                Some(binary::SqliteData::Ready(viewer)) => viewer.selected_table.clone(),
                _ => None,
            },
            _ => None,
        };
        if let Some(table_name) = table_name {
            self.sqlite_query_page(table_name, page, cx);
        }
    }

    fn sqlite_query_page(&mut self, table_name: String, page: usize, cx: &mut Context<Self>) {
        let preview_gen = self.generation;
        let Some(Content::Binary(BinaryState::Ready(ready))) = Some(&mut self.content) else {
            return;
        };
        let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_mut() else {
            return;
        };
        let Some(table_info) = viewer.db.table(&table_name).cloned() else {
            viewer.page_state = binary::SqlitePageState::Failed("This table is unavailable.".to_owned());
            cx.notify();
            return;
        };
        let range = t3_core::files::sqlite::sqlite_page_range(
            table_info.row_count,
            page,
            t3_core::files::sqlite::SQLITE_PAGE_SIZE,
        );
        viewer.query_generation += 1;
        let query_gen = viewer.query_generation;
        viewer.selected_table = Some(table_name.clone());
        viewer.page = range.page;
        viewer.page_state = binary::SqlitePageState::Loading;
        viewer.h_offset = px(0.);
        let db = viewer.db.clone();
        let target_page = range.page;
        viewer.query_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { db.query_page(&table_name, target_page) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != preview_gen {
                    return;
                }
                if let Content::Binary(BinaryState::Ready(ready)) = &mut this.content
                    && let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_mut()
                {
                    viewer.apply_page_result(query_gen, result);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn archive_open_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        if let Content::Binary(state) = &mut self.content {
            state.open_folder(folder);
            cx.notify();
        }
    }

    pub(super) fn archive_go_up(&mut self, cx: &mut Context<Self>) {
        if let Content::Binary(state) = &mut self.content {
            state.go_up();
            cx.notify();
        }
    }

    pub(super) fn archive_open_entry(&mut self, path: String, cx: &mut Context<Self>) {
        if let Content::Binary(state) = &mut self.content {
            state.open_entry(path);
            cx.notify();
        }
    }

    pub(super) fn audio_state(&self) -> Option<&AudioState> {
        match &self.content {
            Content::Audio(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn audio_state_mut(&mut self) -> Option<&mut AudioState> {
        match &mut self.content {
            Content::Audio(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn pdf_state(&self) -> Option<&PdfState> {
        match &self.content {
            Content::Pdf(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn pdf_ready(&self) -> Option<&PdfReady> {
        match &self.content {
            Content::Pdf(PdfState::Ready(ready)) => Some(ready),
            _ => None,
        }
    }

    pub(super) fn pdf_ready_mut(&mut self) -> Option<&mut PdfReady> {
        match &mut self.content {
            Content::Pdf(PdfState::Ready(ready)) => Some(ready),
            _ => None,
        }
    }

    pub(super) fn office_state(&self) -> Option<&OfficeState> {
        match &self.content {
            Content::Office(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn office_ready_mut(&mut self) -> Option<&mut OfficeReady> {
        match &mut self.content {
            Content::Office(OfficeState::Ready(ready)) => Some(ready),
            _ => None,
        }
    }

    /// Moves focus into the opened file's preview/editor (`pingdotgg/t3code#34360`).
    pub fn request_content_focus(&mut self, cx: &mut Context<Self>) {
        self.pending_content_focus = true;
        cx.notify();
    }

    /// Handles `Cmd+F` / `preview.find` inside the open file preview (`#34360`, `#33956`).
    pub fn focus_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.focus_pdf_find(window, cx) {
            return true;
        }
        if self.editor.is_editing() {
            if let Some(editor) = self.editor.editor.clone() {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            return true;
        }
        if matches!(self.content, Content::Text(_)) && !self.is_rendered_active() {
            self.code.open_find();
            if let Some(input) = self.code.find_input.clone() {
                input.update(cx, |state, cx| state.focus(window, cx));
                self.code.find_focus_pending = false;
            }
            cx.notify();
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(super) fn load_pdf_for_test(&mut self, doc_key: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let decoded = pdf::decode(bytes);
        let state = PdfState::from_decoded(doc_key.to_owned(), decoded, &self.pdf_positions);
        self.replace_content(Content::Pdf(state), cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn load_office_for_test(
        &mut self,
        kind: t3_core::files::office::OfficeKind,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let decoded = office::decode(kind, bytes);
        self.replace_content(Content::Office(OfficeState::from_decoded(decoded)), cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn load_text_for_test(&mut self, target: Target, contents: &str, cx: &mut Context<Self>) {
        self._fetch = None;
        let key = Key::of(&target);
        self.target = Some(target);
        self.key = Some(key.clone());
        let file = FileContents {
            relative_path: key.path.clone(),
            contents: contents.to_owned(),
            byte_length: contents.len() as u64,
            truncated: false,
        };
        let built = Rc::new(build_text_file(key, file));
        self.show_text(built, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn load_binary_for_test(&mut self, path: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let decoded = binary::decode(path, bytes);
        self.replace_content(Content::Binary(BinaryState::from(Ok(decoded))), cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn load_font_for_test(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let decoded = font::decode(bytes);
        let state = FontState::from_decoded(decoded, cx);
        self.replace_content(Content::Font(state), cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn load_audio_for_test(&mut self, path: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let info = t3_core::files::media_preview::inspect_audio_bytes(path, &bytes);
        let decoded = audio::DecodedAudio {
            name: t3_core::files::basename(path).to_owned(),
            path: path.to_owned(),
            thread_id: "t-1".into(),
            info,
            bytes,
        };
        self.replace_content(Content::Audio(AudioState::from_decoded(Ok(decoded))), cx);
        cx.notify();
    }

    fn render_markdown(&mut self, file: &Rc<TextFile>, cx: &mut Context<Self>) -> AnyElement {
        let target = self.target.as_ref();
        let state = match &self.markdown {
            Some((key, state)) if *key == file.key => state.clone(),
            _ => {
                let raw = mask_frontmatter(&file.contents);
                let text = t3_core::markdown::prepare_display_markdown_with_raw_html(
                    &raw,
                    t3_core::markdown::DisplayMarkdownOptions {
                        is_dark: cx.theme().is_dark(),
                        cwd: target.map(|target| target.root.workspace.cwd.as_str()),
                        math_enabled: crate::settings::client_settings(cx).math_rendering_enabled(),
                    },
                    true,
                );
                if let Some(document) = &self.media_document { *document.write().unwrap() = std::sync::Arc::new(t3_core::media::MarkdownMediaDocument::new(std::sync::Arc::from(text.as_str()))); }
                else { self.media_document = Some(crate::transcript::media_document(std::sync::Arc::from(text.as_str()))); }
                let state = cx.new(|cx| {
                    TextViewState::markdown(&text, cx).selectable(true)
                });
                #[cfg(feature = "snapshot")]
                if !text.contains("![") { crate::snapshot::record_media_prose(&state, &text, cx); }
                self.markdown = Some((file.key.clone(), state.clone()));
                state
            }
        };
        let scope = crate::media::MediaScope {
            owner: cx.entity_id(),
            thread_id: target.map(|target| target.root.workspace.thread_id.clone()),
            workspace_root: target.map(|target| t3_core::media::markdown_image_base_dir(&target.root.workspace.cwd, &target.path)),
            workspace_mutation_id: target.and(self.mutation.clone()),
            source_control_media: false,
            source_control_media_repository: None,
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
                    .child(div().text_color(hsla(p.destructive)).child(crate::i18n::t(cx, &failure.describe()).to_owned()))
                    .children(failure.attempted_path.clone().map(|path| {
                        v_flex()
                            .text_color(hsla(p.muted_foreground))
                            .child("Attempted path")
                            .child(div().font_family(mono.clone()).text_color(hsla(p.foreground)).child(path))
                    }))
                    .when(target.is_some() && !host, |this| {
                        this.child(
                            div()
                                .text_color(hsla(p.muted_foreground))
                                .child(format!("Workspace folder: {cwd}. Check the link's path or locate the file in Files.")),
                        )
                    })
                    .when(self.attachment_resource().is_some(), |this| {
                        this.child(super::Button::new("attachment-retry")
                            .label(crate::i18n::t(cx, "Retry"))
                            .on_click(cx.listener(|this, _, _, cx| this.retry_attachment(cx))))
                    }),
            )
            .into_any_element()
    }

    fn render_html(&mut self, file: &Rc<TextFile>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use crate::webview::{Layer, Page, WebView, web_view};

        let dark = cx.theme().is_dark();
        if self._activation.is_none() {
            self._activation = Some(cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() && this.html.is_none() {
                    cx.notify();
                }
            }));
        }
        if self.html.as_ref().is_some_and(|(current, _)| !Rc::ptr_eq(current, file)) {
            self.html = None;
        }
        if self.html.is_none() && window.is_window_active() {
            let page = Page {
                html: Arc::from(file.contents.as_str()),
                dark,
                allow_popups: false,
                scroll_through: false,
                script: String::new(),
                content_security_policy: Some(
                    "default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; form-action 'none'".into(),
                ),
            };
            let (events, _) = futures::channel::mpsc::unbounded();
            self.html = WebView::new(page, Layer::Transcript, events, window, cx)
                .map(|view| (file.clone(), view));
        }
        self.html.as_ref().map_or_else(
            || div().flex_1().items_center().justify_center().flex().text_xs()
                .text_color(hsla(palette(cx).muted_foreground))
                .child(crate::i18n::t(cx, "Unable to load file preview.")).into_any_element(),
            |(_, view)| div().flex_1().min_h_0().overflow_hidden()
                .child(web_view("file-html-preview", view.clone(), dark)).into_any_element(),
        )
    }
}

fn is_html(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
    })
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
        if std::mem::take(&mut self.pending_content_focus) {
            if self.editor.is_editing()
                && let Some(editor) = self.editor.editor.clone()
            {
                let focus = editor.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            } else {
                window.focus(&self.content_focus, cx);
            }
        }
        let body = match &self.content {
            Content::Empty | Content::Folder => div().into_any_element(),
            Content::Loading => loading(cx),
            Content::Failed(failure) => self.render_failure(&failure.clone(), cx),
            Content::Image(image) => image.render(cx),
            Content::Font(font) => font.render(cx),
            Content::Audio(_) => self.render_audio(cx),
            Content::Pdf(_) => self.render_pdf(window, cx),
            Content::Office(_) => self.render_office(cx),
            Content::Binary(_) => self.render_binary(cx),
            Content::Video { player, .. } => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .child(player.clone())
                .into_any_element(),
            Content::Text(file) => {
                let file = file.clone();
                if self.editor.is_editing() {
                    self.render_editor(window, cx)
                } else if self.is_rendered_active() {
                    match self.rendered_mode() {
                        Some(RenderedMode::Markdown) => self.render_markdown(&file, cx),
                        Some(RenderedMode::Mermaid) => self.render_mermaid(&file.key, &file.contents, cx),
                        Some(RenderedMode::Table) => {
                            let sep = delimited_separator(&file.key.path).unwrap_or(',');
                            self.render_table(&file.key, &file.contents, sep, cx)
                        }
                        Some(RenderedMode::Notebook) => self.render_notebook(&file.key, &file.contents, cx),
                        Some(RenderedMode::Html) => self.render_html(&file, window, cx),
                        None => self.render_code(window, cx),
                    }
                } else {
                    self.render_code(window, cx)
                }
            }
        };
        v_flex()
            .id("file-preview-content")
            .key_context("Preview")
            .track_focus(&self.content_focus)
            .on_action(cx.listener(|this, _: &crate::preview::Find, window, cx| {
                if !this.focus_find(window, cx) {
                    cx.propagate();
                }
            }))
            .relative()
            .size_full()
            .min_h_0()
            .child(body)
            .child(self.viewer.clone())
    }
}

#[cfg(feature = "snapshot")]
#[derive(Clone, serde::Deserialize)]
pub(crate) struct MarkdownFixture { pub path: String, pub text: String }
#[cfg(feature = "snapshot")]
impl gpui::Global for MarkdownFixture {}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_target(path: &str) -> Target {
        Target {
            root: Root {
                environment: "env-1".into(),
                workspace: super::super::FileWorkspace {
                    thread_id: "t-1".into(),
                    cwd: "/repo".into(),
                    project_name: "repo".into(),
                },
            },
            path: path.into(),
            reveal_line: None,
            reveal_request_id: 0,
        }
    }

    #[gpui::test]
    fn video_file_opens_workspace_video_player(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let preview = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(runtime.handle().clone(), cx));
            cx.new(|cx| Preview::new(state, cx))
        });
        preview.update(cx, |preview, cx| {
            preview.set_target(Some(test_target("demo/clip.mp4")), cx);
            match &preview.content {
                Content::Video { source, .. } => assert_eq!(
                    *source,
                    t3_core::media::MediaSource::Asset(t3_core::media::AssetResource::MediaFile {
                        thread_id: "t-1".into(),
                        path: "demo/clip.mp4".into(),
                    })
                ),
                _ => panic!("expected Content::Video for demo/clip.mp4"),
            }
        });
    }

    #[gpui::test]
    fn rich_previews_and_editor_dismissal_guard_render_cleanly(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (preview, vcx) = cx.add_window_view(|_, cx| Preview::new(state.clone(), cx));

        // 1. Delimited CSV table preview
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_text_for_test(test_target("data.csv"), "name,val\nalpha,10\nbeta,2\n", cx);
                assert_eq!(p.rendered_mode(), Some(RenderedMode::Table));
                assert!(p.is_rendered_active());
            });
            window.draw(cx).clear();
        });
        preview.update(vcx, |p, _| {
            let table = p.table.table.as_mut().expect("table parsed");
            assert_eq!(table.row_count(), 2);
            table.toggle_sort(1);
            assert_eq!(table.display_row(0).unwrap().1[0], "beta");
        });

        // 2. Jupyter notebook preview
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                let nb = serde_json::json!({
                    "nbformat": 4,
                    "metadata": { "language_info": { "name": "python" } },
                    "cells": [
                        { "cell_type": "markdown", "source": ["# Notes"] },
                        { "cell_type": "code", "execution_count": 1, "source": ["print(1)"], "outputs": [{ "output_type": "stream", "text": ["1\n"] }] }
                    ]
                });
                p.load_text_for_test(test_target("demo.ipynb"), &nb.to_string(), cx);
                assert_eq!(p.rendered_mode(), Some(RenderedMode::Notebook));
            });
            window.draw(cx).clear();
        });

        // 3. Mermaid file preview (valid + unsafe directive guard)
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_text_for_test(test_target("arch.mmd"), "flowchart LR\n  A --> B", cx);
                assert_eq!(p.rendered_mode(), Some(RenderedMode::Mermaid));
            });
            window.draw(cx).clear();
        });
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_text_for_test(test_target("unsafe.mmd"), "flowchart LR\n  A[https://evil.test] --> B", cx);
            });
            window.draw(cx).clear();
        });

        // 4. Binary / Hex preview
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_binary_for_test("blob.bin", b"\x89PNG\r\n\x1a\nhello binary!".to_vec(), cx);
            });
            window.draw(cx).clear();
        });

        // 5. Audio & Font previews
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_audio_for_test("clip.wav", b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec(), cx);
            });
            window.draw(cx).clear();
        });
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_font_for_test(b"not-valid-font".to_vec(), cx);
            });
            window.draw(cx).clear();
        });

        // 6. PDF preview + progressive Find (`#33956`) and Office spreadsheet preview + row/col resize (`#35548`)
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                let sample_pdf = b"%PDF-1.4\n1 0 obj << /Type /Page /MediaBox [0 0 612 792] /Contents 2 0 R >> endobj\n2 0 obj << /Length 36 >>\nstream\nBT /F1 18 Tf (Quarterly needle) Tj ET\nendstream\nendobj\n";
                p.load_pdf_for_test("env:repo:report.pdf", sample_pdf.to_vec(), cx);
                assert!(p.pdf_ready().is_some());
            });
            window.draw(cx).clear();
            preview.update(cx, |p, cx| {
                assert!(p.focus_find(window, cx));
                let find_input = p.pdf_ready().and_then(|r| r.find_input.clone()).expect("pdf find input");
                assert!(find_input.read(cx).focus_handle(cx).is_focused(window));
                // Blur before `TestWindow` repaints (`gpui-component` calls `set_text_content_type` on focused inputs).
                window.blur();
            });
        });
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_office_for_test(t3_core::files::office::OfficeKind::Rtf, br"{\rtf1\ansi Hello\par World}".to_vec(), cx);
                assert!(p.office_ready_mut().is_some());
            });
            window.draw(cx).clear();
        });
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                let wb = t3_core::files::office::OfficeWorkbook {
                    sheets: vec![t3_core::files::office::OfficeSheet {
                        name: "Data".into(),
                        rows: vec![vec!["Item".into(), "42".into()]],
                        column_count: 2,
                        col_widths: vec![100.0, 100.0],
                        row_heights: vec![24.0],
                        frozen_rows: 0,
                        frozen_cols: 0,
                        show_gridlines: true,
                        truncated: false,
                    }],
                    truncated: false,
                };
                p.replace_content(
                    Content::Office(OfficeState::from_decoded(Ok(t3_core::files::office::OfficePreview::Workbook(wb)))),
                    cx,
                );
                let ready = p.office_ready_mut().expect("office workbook ready");
                if let t3_core::files::office::OfficePreview::Workbook(wb) = &mut ready.preview {
                    assert_eq!(wb.sheets[0].set_col_width(0, 180.0).unwrap(), 180.0);
                    assert_eq!(wb.sheets[0].set_row_height(0, 48.0).unwrap(), 48.0);
                } else {
                    panic!("expected workbook preview");
                }
            });
            window.draw(cx).clear();
        });

        // 7. Read-only code preview Cmd+F opens in-file find bar and matches lines (`#34360`)
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_text_for_test(test_target("src/find_me.rs"), "fn alpha() {}\nfn beta_target() {}\n", cx);
                p.request_content_focus(cx);
                p.code.find_open = true;
                p.code.find_query = "beta".into();
                p.code.recompute_find();
                assert_eq!(p.code.find_matches, vec![1]);
            });
            window.draw(cx).clear();
            preview.update(cx, |p, cx| {
                assert!(p.content_focus.is_focused(window));
                assert!(p.focus_find(window, cx));
                let input = p.code.find_input.clone().expect("code find input");
                assert!(input.read(cx).focus_handle(cx).is_focused(window));
                input.update(cx, |state, cx| state.set_value("beta", window, cx));
                assert_eq!(input.read(cx).value().as_str(), "beta");
                p.code.close_find(window, cx);
                assert!(!p.code.find_open);
                assert!(input.read(cx).value().is_empty(), "closing code find must clear the input text");
                window.blur();
            });
        });

        // 8. In-place editor + dirty state + unsaved dismissal guard + discard
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_text_for_test(test_target("src/lib.rs"), "pub fn hi() {}\n", cx);
                p.enter_edit_mode(cx);
                assert!(p.is_editing());
                assert!(!p.is_dirty());
                p.on_editor_text_changed("pub fn hi() { /* edited */ }\n".into(), cx);
                assert!(p.is_dirty());
                p.request_exit_edit_mode(cx);
                assert!(p.is_editing(), "dirty editor must remain open with dismissal guard");
                assert!(p.editor.has_unsaved_prompt());
            });
            window.draw(cx).clear();
        });
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.discard_unsaved_and_exit(cx);
                assert!(!p.is_editing());
                assert!(!p.is_dirty());
            });
            window.draw(cx).clear();
        });

        #[cfg(feature = "snapshot")]
        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    fn sqlite_preview_renders_tables_pages_generated_columns_hex_toggle_and_errors(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let (preview, vcx) = cx.add_window_view(|_, cx| Preview::new(state.clone(), cx));

        // 1. Multi-table DB with >50 rows, GENERATED ALWAYS AS columns, WITHOUT ROWID, BLOB & NULL.
        let mut sql = String::from(
            "CREATE TABLE a_items (id INTEGER PRIMARY KEY, label TEXT, raw BLOB);
             CREATE TABLE z_orders (
                 price INTEGER,
                 qty INTEGER,
                 total INTEGER GENERATED ALWAYS AS (price * qty) STORED,
                 doubled INTEGER GENERATED ALWAYS AS (price * 2) VIRTUAL
             );
             INSERT INTO z_orders(price, qty) VALUES (7, 3);
             CREATE TABLE z_empty (k TEXT PRIMARY KEY) WITHOUT ROWID;",
        );
        for idx in 1..=62 {
            if idx == 1 {
                sql.push_str("INSERT INTO a_items VALUES (1, NULL, X'CAFEBABE');");
            } else {
                sql.push_str(&format!("INSERT INTO a_items VALUES ({idx}, 'item-{idx}', NULL);"));
            }
        }
        let db_bytes = super::super::sqlite::build_test_sqlite_bytes(&sql);

        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_binary_for_test("data/app.sqlite", db_bytes, cx);
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                assert_eq!(ready.mode, BinaryViewMode::Sqlite);
                let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_ref() else {
                    panic!("expected ready sqlite viewer");
                };
                assert_eq!(viewer.selected_table.as_deref(), Some("a_items"));
                assert_eq!(viewer.page, 0);
                let binary::SqlitePageState::Ready(page0) = &viewer.page_state else {
                    panic!("expected initial page 0 ready");
                };
                assert_eq!(page0.rows.len(), 50);
                assert_eq!(page0.rows[0][1].display, "NULL");
                assert_eq!(page0.rows[0][2].display, "[blob: 4 bytes · ca fe ba be]");
            });
            window.draw(cx).clear();
        });

        // 2. Navigate to page 1 on `a_items` (rows 51..=62).
        vcx.update(|_, cx| {
            preview.update(cx, |p, cx| p.sqlite_select_page(1, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            preview.update(cx, |p, _| {
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_ref() else {
                    panic!("expected ready sqlite viewer");
                };
                assert_eq!(viewer.page, 1);
                let binary::SqlitePageState::Ready(page1) = &viewer.page_state else {
                    panic!("expected page 1 ready");
                };
                assert_eq!(page1.rows.len(), 12);
                assert_eq!(page1.rows[0][0].display, "51");
                assert_eq!(page1.rows[11][0].display, "62");
            });
            window.draw(cx).clear();
        });

        // 3. Switch to `z_orders` and verify STORED & VIRTUAL generated columns.
        vcx.update(|_, cx| {
            preview.update(cx, |p, cx| p.sqlite_select_table("z_orders".into(), cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            preview.update(cx, |p, _| {
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_ref() else {
                    panic!("expected ready sqlite viewer");
                };
                assert_eq!(viewer.selected_table.as_deref(), Some("z_orders"));
                let binary::SqlitePageState::Ready(page) = &viewer.page_state else {
                    panic!("expected z_orders page ready");
                };
                let cols: Vec<&str> = page.columns.iter().map(|c| c.name.as_str()).collect();
                assert_eq!(cols, vec!["price", "qty", "total", "doubled"]);
                assert_eq!(page.rows[0][2].display, "21");
                assert_eq!(page.rows[0][3].display, "14");
            });
            window.draw(cx).clear();
        });

        // 4. Switch to empty WITHOUT ROWID table and toggle Hex <-> Database cleanly.
        vcx.update(|_, cx| {
            preview.update(cx, |p, cx| p.sqlite_select_table("z_empty".into(), cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear();
            preview.update(cx, |p, cx| {
                p.set_binary_mode(BinaryViewMode::Hex, cx);
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                assert_eq!(ready.mode, BinaryViewMode::Hex);
            });
            window.draw(cx).clear();
            preview.update(cx, |p, cx| {
                p.set_binary_mode(BinaryViewMode::Sqlite, cx);
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                assert_eq!(ready.mode, BinaryViewMode::Sqlite);
            });
            window.draw(cx).clear();
        });

        // 5. Corrupt / encrypted `.sqlite` shows error in Database mode and still allows Hex inspection.
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_binary_for_test("broken.sqlite", b"encrypted-or-corrupt-payload".to_vec(), cx);
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state for corrupt sqlite");
                };
                assert_eq!(ready.mode, BinaryViewMode::Sqlite);
                assert!(matches!(ready.sqlite, Some(binary::SqliteData::Failed(_))));
            });
            window.draw(cx).clear();
            preview.update(cx, |p, cx| p.set_binary_mode(BinaryViewMode::Hex, cx));
            window.draw(cx).clear();
        });

        // 6. Empty database (0 tables) renders cleanly.
        let empty_db = super::super::sqlite::build_test_sqlite_bytes("CREATE TABLE tmp (x INT); DROP TABLE tmp;");
        vcx.update(|window, cx| {
            preview.update(cx, |p, cx| {
                p.load_binary_for_test("empty.db", empty_db, cx);
                let Some(BinaryState::Ready(ready)) = p.binary_state() else {
                    panic!("expected ready binary state");
                };
                let Some(binary::SqliteData::Ready(viewer)) = ready.sqlite.as_ref() else {
                    panic!("expected ready sqlite viewer");
                };
                assert!(viewer.db.tables().is_empty());
            });
            window.draw(cx).clear();
        });

        #[cfg(feature = "snapshot")]
        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }
}
