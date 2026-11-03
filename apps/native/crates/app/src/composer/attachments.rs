//! Composer attachments: paste, drop and the paperclip (`useComposerAttachmentIntake.ts`,
//! `ComposerAttachButton`), the upload queue (`web/src/lib/attachmentUploadQueue.ts`), large
//! pastes folded into a file, and the strip of thumbnails and file rows
//! (`ComposerAttachmentStrip.tsx`). Classification, limits and payloads live in
//! `t3_core::composer::attachments`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClipboardEntry, Context, ExternalPaths, Image, ImageFormat, IntoElement, ObjectFit, ParentElement,
    PathPromptOptions, SharedString, Styled, StyledImage, Task, Window, div, img, prelude::*, px,
};
use gpui_component::h_flex;
use t3_core::composer::attachments::{
    self as rules, Attachment, AttachmentKind, Classified, Upload,
};
use t3_core::composer::menu::serialize_file_link;

use super::{Composer, Target};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// One staged attachment and what its tile needs.
pub(super) struct Staged {
    pub(super) attachment: Attachment,
    preview: Option<Arc<Image>>,
    /// The bytes, kept until the upload lands so a failed one can retry.
    bytes: Option<Arc<Vec<u8>>>,
    _upload: Option<Task<()>>,
    source: Option<String>,
}

#[derive(Default)]
pub(super) struct Attachments {
    staged: HashMap<Target, Vec<Staged>>,
}

/// A file read for staging.
struct Picked {
    name: String,
    bytes: Vec<u8>,
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).filter(|name| !name.is_empty()).unwrap_or_else(|| "file".to_owned())
}

fn image_format(mime_type: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(mime_type)
}

impl Composer {
    /// The open target's attachments; none while a queued message is open for editing.
    pub(super) fn staged(&self) -> &[Staged] {
        if self.editing_queued().is_some() {
            return &[];
        }
        self.snapshot.target.as_ref().map(|target| self.staged_for(target)).unwrap_or_default()
    }

    pub(super) fn staged_for(&self, target: &Target) -> &[Staged] {
        self.attachments.staged.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    /// Attachments a saved draft restores: uploaded, without their bytes or preview.
    pub(super) fn seed_attachments(&mut self, target: &Target, restored: Vec<Attachment>) {
        if restored.is_empty() {
            return;
        }
        let staged = restored.into_iter().map(|attachment| Staged { attachment, preview: None, bytes: None, _upload: None, source: None });
        self.attachments.staged.entry(target.clone()).or_default().extend(staged);
    }

    pub(super) fn stage_imported_attachment(&mut self, target: &Target, imported: super::imports::ImportedAttachment, cx: &mut Context<Self>) {
        let id=imported.attachment.id.clone();
        let copying=imported.source.is_some();
        self.attachments.staged.entry(target.clone()).or_default().push(Staged {attachment:imported.attachment, preview:None, bytes:None, _upload:None, source:imported.source});
        if copying {self.start_upload(target.clone(),id,cx);}
    }

    pub(super) fn staged_attachments(&self) -> Vec<Attachment> {
        self.staged().iter().map(|staged| staged.attachment.clone()).collect()
    }

    /// `fileAttachmentStagingLimit`: the largest file the server takes, `None` for no files.
    fn file_limit(&self, cx: &App) -> Option<u64> {
        let config = self.state.read(cx).server_config.config.as_ref()?;
        rules::file_staging_limit(config.environment.capabilities.file_attachments.map(|files| files.max_upload_bytes))
    }

    /// Attachments go with a message, not with an answer to the agent's question.
    fn can_attach(&self) -> bool {
        self.snapshot.target.is_some()
            && self.answering.is_none()
            && self.snapshot.pending.user_inputs.is_empty()
            && self.editing_queued().is_none()
    }

    /// `showComposerAttachAction`.
    pub(super) fn shows_attach_button(&self, cx: &App) -> bool {
        self.can_attach() && self.file_limit(cx).is_some()
    }

    /// The paperclip: a file dialog for any number of files.
    pub(super) fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.attach_paths(paths, window, cx);
                this.focus(window, cx);
            });
        })
        .detach();
    }

    /// Files dropped, pasted from Finder, or picked. Folders become mentions when the server
    /// runs on this machine, as the web's `addComposerFolders` allows.
    pub(super) fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach() {
            return;
        }
        let (folders, files): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|path| path.is_dir());
        if !folders.is_empty() {
            let local = self.state.read(cx).environment().is_some_and(rules::is_local_server);
            if local {
                for folder in &folders {
                    let text = format!("{} ", serialize_file_link(&folder.to_string_lossy()));
                    self.append_to_prompt(&text, window, cx);
                }
            } else {
                self.error = Some("Folders can only be referenced on a server running on this computer".to_owned());
                cx.notify();
            }
        }
        if files.is_empty() {
            return;
        }
        let Some(target) = self.snapshot.target.clone() else { return };
        let read = cx.background_executor().spawn(async move {
            files
                .into_iter()
                .map(|path| match std::fs::read(&path) {
                    Ok(bytes) => Ok(Picked { name: file_name(&path), bytes }),
                    Err(_) => Err(format!("'{}' is empty or could not be read.", file_name(&path))),
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let picked = read.await;
            let _ = this.update(cx, |this, cx| {
                for file in picked {
                    match file {
                        Ok(file) => this.stage(&target, file.name, file.bytes, false, cx),
                        Err(error) => this.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Admits one file (`addComposerAttachments`) and starts its upload.
    fn stage(&mut self, target: &Target, name: String, bytes: Vec<u8>, pasted_text: bool, cx: &mut Context<Self>) {
        let classified = if pasted_text { Classified::File { mime_type: "text/plain;charset=utf-8".to_owned() } } else { rules::classify(&name) };
        let staged = self.attachments.staged.get(target).map_or(0, Vec::len);
        let size = bytes.len() as u64;
        if let Some(error) = rules::admission_error(staged, &name, &classified, size, self.file_limit(cx)) {
            self.error = Some(error);
            return;
        }
        let (kind, mime_type) = match classified {
            Classified::Image { mime_type } => (AttachmentKind::Image, mime_type.to_owned()),
            Classified::File { mime_type } => (AttachmentKind::File, mime_type),
            Classified::UnsupportedImage => return,
        };
        let preview = (kind == AttachmentKind::Image)
            .then(|| image_format(&mime_type))
            .flatten()
            .map(|format| Arc::new(Image::from_bytes(format, bytes.clone())));
        let attachment = Attachment { id: uuid_v4(), kind, name, mime_type, size_bytes: size, pasted_text, upload: Upload::Uploading };
        let id = attachment.id.clone();
        self.attachments.staged.entry(target.clone()).or_default().push(Staged {
            attachment,
            preview,
            bytes: Some(Arc::new(bytes)),
            _upload: None,
            source: None,
        });
        self.record_draft(target, cx);
        self.start_upload(target.clone(), id, cx);
    }

    fn find_staged(&mut self, target: &Target, id: &str) -> Option<&mut Staged> {
        self.attachments.staged.get_mut(target)?.iter_mut().find(|staged| staged.attachment.id == id)
    }

    /// Mints an upload and posts the bytes on the network runtime.
    pub(super) fn start_upload(&mut self, target: Target, id: String, cx: &mut Context<Self>) {
        let (client, runtime) = {
            let state = self.state.read(cx);
            (state.client_handle(), state.runtime().clone())
        };
        let Some(staged) = self.find_staged(&target, &id) else { return };
        let bytes = staged.bytes.clone();
        let source = staged.source.clone();
        if bytes.is_none() && source.is_none() { return; }
        let attachment = staged.attachment.clone();
        staged.attachment.upload = Upload::Uploading;
        let Some(client) = client else {
            staged.attachment.upload = Upload::Failed { reason: "Not connected".to_owned() };
            return;
        };
        let request = staged.attachment.upload_request();
        let mime_type = staged.attachment.mime_type.clone();
        let (task_target, task_id) = (target.clone(), id.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn(async move { match source {
                    Some(source)=>t3_client::upload::copy_attachment_owned(client,&attachment,&source).await,
                    None=>t3_client::upload::upload_attachment_owned(client, request, &mime_type, bytes.expect("upload bytes").to_vec()).await,
                } })
                .await
                .unwrap_or_else(|_| Err("Upload failed".to_owned()));
            let _ = this.update(cx, |this, cx| {
                let Some(staged) = this.find_staged(&task_target, &task_id) else { return };
                match result {
                    Ok(attachment_id) => {
                        staged.attachment.upload = Upload::Ready { attachment_id: attachment_id.into_id() };
                        staged.bytes = None;
                        this.record_draft(&task_target, cx);
                    }
                    Err(reason) => staged.attachment.upload = Upload::Failed { reason },
                }
                cx.notify();
            });
        });
        if let Some(staged) = self.find_staged(&target, &id) {
            staged._upload = Some(task);
        }
    }

    pub(super) fn release_upload(&self, attachment_id: &str, cx: &App) {
        let request = self.state.read(cx).rpc(rules::DELETE, rules::delete_request(attachment_id));
        cx.background_executor().spawn(async move { request.await.ok() }).detach();
    }

    pub(super) fn retry_upload(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(target) = self.snapshot.target.clone() {
            self.start_upload(target, id, cx);
            cx.notify();
        }
    }

    /// Removes a tile. An uploaded file the message never carried is deleted on the server.
    pub(super) fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let Some(staged) = self.attachments.staged.get_mut(&target) else { return };
        let Some(index) = staged.iter().position(|staged| staged.attachment.id == id) else { return };
        let removed = staged.remove(index);
        if let Some(attachment_id) = removed.attachment.attachment_id() {
            self.release_upload(attachment_id, cx);
        }
        self.draft_changed(cx);
        cx.notify();
    }

    /// Takes the target's attachments for a send; `restore_attachments` puts them back if it fails.
    pub(super) fn take_attachments(&mut self, target: &Target) -> Vec<Staged> {
        self.attachments.staged.remove(target).unwrap_or_default()
    }

    pub(super) fn restore_attachments(&mut self, target: &Target, mut taken: Vec<Staged>) {
        let current = self.attachments.staged.entry(target.clone()).or_default();
        taken.append(current);
        *current = taken;
    }

    /// Paste: images and copied files attach; a large text paste becomes a file. Returns whether
    /// the paste was handled, so plain text still reaches the prompt.
    pub(super) fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        if !self.can_attach() {
            return false;
        }
        let Some(target) = self.snapshot.target.clone() else { return false };
        if let (Some(text), Some(fragment)) = (item.text(), item.metadata())
            && self.paste_context(&text, fragment, window, cx) { return true; }
        if let (Some(text), Some(fragment)) = (item.text(), super::clipboard::read_html_fragment())
            && self.paste_context(&text, &fragment, window, cx) { return true; }
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
                    self.attach_paths(paths.paths().to_vec(), window, cx);
                    return true;
                }
                ClipboardEntry::Image(image) => {
                    let format = image.format;
                    if rules::image_mime_for_format(format.extension()).is_none() {
                        self.error = Some(rules::unsupported_image_message(&format!("image.{}", format.extension())));
                    } else {
                        self.stage(&target, format!("image.{}", format.extension()), image.bytes.clone(), false, cx);
                    }
                    cx.notify();
                    return true;
                }
                _ => {}
            }
        }
        let Some(text) = item.text() else { return false };
        self.fold_paste(&target, text, cx)
    }

    /// `foldPastedText`.
    fn fold_paste(&mut self, target: &Target, text: String, cx: &mut Context<Self>) -> bool {
        let selection = self.input.read(cx).selected_range();
        let exceeds = rules::paste_exceeds_limit(&self.prompt(cx), selection, &text);
        if !rules::folds_into_file(&text, false, exceeds) {
            return false;
        }
        let staged = self.attachments.staged.get(target).map_or(0, Vec::len);
        let limit = self.file_limit(cx).filter(|_| staged < rules::MAX_ATTACHMENTS);
        let toast = |state: &mut crate::app_state::AppState, title: &str, description: &str, cx: &mut Context<crate::app_state::AppState>| {
            state.show_toast(Toast::new(ToastKind::Error, title.to_owned()).description(description.to_owned()), cx)
        };
        let Some(limit) = limit else {
            if exceeds {
                let (title, description) = ("Pasted text is too large for this message", "Remove some text or an attachment, then paste again.");
                self.state.update(cx, |state, cx| toast(state, title, description, cx));
                return true;
            }
            return false;
        };
        if text.len() as u64 > limit {
            if exceeds {
                let (title, description) = ("Pasted text is too large to attach", "Reduce the clipboard contents or save a smaller excerpt as a file.");
                self.state.update(cx, |state, cx| toast(state, title, description, cx));
                return true;
            }
            return false;
        }
        let names: Vec<String> = self.attachments.staged.get(target).into_iter().flatten().map(|staged| staged.attachment.name.clone()).collect();
        let name = rules::next_pasted_text_name(names.iter().map(String::as_str));
        let size = rules::format_size(text.len() as u64);
        self.stage(target, name.clone(), text.into_bytes(), true, cx);
        let description = format!("{size} · Use ⌘⇧V to keep a large paste inline.");
        self.state.update(cx, |state, cx| {
            state.show_toast(Toast::new(ToastKind::Success, format!("Large paste attached as {name}")).description(description), cx)
        });
        cx.notify();
        true
    }

    /// ⌘⇧V: the clipboard's text goes into the prompt as is, however large.
    pub(super) fn paste_as_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        self.input.update(cx, |input, cx| input.insert(text, window, cx));
    }

    fn append_to_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt(cx);
        let boundary = if prompt.is_empty() || prompt.ends_with(char::is_whitespace) { "" } else { " " };
        let end = prompt.len();
        self.input.update(cx, |input, cx| {
            input.set_selected_range(end..end, cx);
            input.replace(format!("{boundary}{text}"), window, cx);
        });
    }

    /// `ComposerAttachmentMediaRow` and `ComposerAttachmentFileList`.
    pub(super) fn render_attachments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let staged = self.staged();
        if staged.is_empty() {
            return None;
        }
        let p = palette(cx);
        let images: Vec<&Staged> = staged.iter().filter(|staged| staged.attachment.kind == AttachmentKind::Image).collect();
        let files: Vec<&Staged> = staged.iter().filter(|staged| staged.attachment.kind == AttachmentKind::File).collect();
        let tiles = (!images.is_empty()).then(|| {
            h_flex().flex_wrap().gap(px(8.)).mb(px(12.)).children(images.into_iter().map(|staged| {
                let attachment = &staged.attachment;
                let id = attachment.id.clone();
                let thumbnail = match &staged.preview {
                    Some(image) => img(image.clone()).size_full().object_fit(ObjectFit::Cover).into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(4.))
                        .text_size(px(10.))
                        .text_color(hsla(p.secondary_label))
                        .child(SharedString::from(attachment.name.clone()))
                        .into_any_element(),
                };
                div()
                    .id(SharedString::from(format!("attachment-{}", attachment.id)))
                    .relative()
                    .size(px(64.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.8))
                    .bg(hsla(p.background))
                    .child(thumbnail)
                    .when(attachment.upload == Upload::Uploading, |this| {
                        this.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .px(px(4.))
                                .bg(hsla(p.background).opacity(0.85))
                                .text_size(px(10.))
                                .text_center()
                                .text_color(hsla(p.foreground))
                                .child("Uploading"),
                        )
                    })
                    .when_some(failed_reason(attachment), |this, reason| {
                        let id = id.clone();
                        this.child(
                            div().absolute().bottom(px(4.)).left(px(4.)).child(
                                media_button(SharedString::from(format!("retry-{}", attachment.id)), Icon::RefreshCw, cx)
                                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                                    .on_click(cx.listener(move |this, _, _, cx| this.retry_upload(id.clone(), cx))),
                            ),
                        )
                    })
                    .child(
                        div().absolute().top(px(4.)).right(px(4.)).child(
                            media_button(SharedString::from(format!("remove-{}", attachment.id)), Icon::X, cx)
                                .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(&id, cx))),
                        ),
                    )
            }))
        });
        let rows = (!files.is_empty()).then(|| {
            gpui_component::v_flex().gap(px(4.)).mb(px(12.)).children(files.into_iter().map(|staged| {
                let attachment = &staged.attachment;
                let id = attachment.id.clone();
                let status = match &attachment.upload {
                    Upload::Uploading => "Uploading".to_owned(),
                    _ => rules::format_size(attachment.size_bytes),
                };
                h_flex()
                    .min_w_0()
                    .gap(px(8.))
                    .py(px(4.))
                    .text_sm()
                    .text_color(hsla(p.foreground))
                    .child(Icon::File.render(px(16.), hsla(p.muted_foreground)))
                    .child(div().flex_1().min_w_0().truncate().child(SharedString::from(attachment.name.clone())))
                    .child(div().flex_shrink_0().text_xs().text_color(hsla(p.secondary_label)).child(status))
                    .when_some(failed_reason(attachment), |this, reason| {
                        let id = id.clone();
                        this.child(
                            ghost_icon_button(SharedString::from(format!("retry-{}", attachment.id)), Icon::RefreshCw, cx)
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| this.retry_upload(id.clone(), cx))),
                        )
                    })
                    .child(
                        ghost_icon_button(SharedString::from(format!("remove-{}", attachment.id)), Icon::X, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(&id, cx))),
                    )
            }))
        });
        Some(gpui_component::v_flex().children(tiles).children(rows).into_any_element())
    }

    /// Drop target for files from Finder.
    pub(super) fn on_drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        self.attach_paths(paths.paths().to_vec(), window, cx);
        self.focus(window, cx);
    }
}

fn failed_reason(attachment: &Attachment) -> Option<SharedString> {
    match &attachment.upload {
        Upload::Failed { reason } => Some(SharedString::from(reason.clone())),
        _ => None,
    }
}

/// The `media-close` button on a thumbnail: a small dark circle (`icon-xs`, 24).
fn media_button(id: SharedString, icon: Icon, _cx: &App) -> gpui::Stateful<gpui::Div> {
    let white = gpui::white();
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(gpui::black().opacity(0.65))
        .border_1()
        .border_color(white.opacity(0.2))
        .cursor_pointer()
        .hover(|this| this.bg(gpui::black().opacity(0.8)))
        .child(icon.render(px(14.), white))
}

/// A ghost `icon-xs` button for a file row.
fn ghost_icon_button(id: SharedString, icon: Icon, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    div()
        .id(id)
        .size(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|this| this.bg(hsla(p.accent)))
        .child(icon.render(px(14.), hsla(p.muted_foreground)))
}


impl Composer {
    pub(super) fn take_draft_attachments(&mut self, target: &Target) -> Vec<Staged> {
        let mut staged = self.attachments.staged.remove(target).unwrap_or_default();
        for entry in &mut staged { entry._upload = None; }
        staged
    }
    pub(super) fn restore_draft_attachments(&mut self, target: &Target, staged: Vec<Staged>, cx: &mut Context<Self>) {
        let retry: Vec<_> = staged.iter().filter(|entry| entry.attachment.attachment_id().is_none()).map(|entry| entry.attachment.id.clone()).collect();
        self.attachments.staged.insert(target.clone(), staged);
        for id in retry { self.start_upload(target.clone(), id, cx); }
    }
}
