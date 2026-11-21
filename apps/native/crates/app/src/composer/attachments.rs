//! Composer attachments: paste, drop and the paperclip (`useComposerAttachmentIntake.ts`,
//! `ComposerAttachButton`), the upload queue (`web/src/lib/attachmentUploadQueue.ts`), large
//! pastes folded into a file, and the strip of thumbnails and file rows
//! (`ComposerAttachmentStrip.tsx`, size labels from `composerContextPresentation.tsx`). Classification, limits and payloads live in
//! `t3_core::composer::attachments`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;

use gpui::{
    AnyElement, App, ClipboardEntry, Context, ExternalPaths, Image, ImageFormat, IntoElement, ObjectFit, ParentElement,
    PathPromptOptions, SharedString, Styled, StyledImage, Task, Window, div, img, prelude::*, px,
};
use gpui_component::h_flex;
use t3_client::upload::{QueueEvent, UploadPayload};
use t3_core::composer::attachments::{
    self as rules, Attachment, AttachmentKind, Upload,
    image as image_rules,
    queue::{AttachmentUploadState, DraftTarget, format_upload_progress},
};
use t3_core::composer::menu::serialize_file_link;

use super::{Composer, Target};
use t3_core::requests::attachments::{IntakePhase, QUESTION_ATTACHMENTS_UNSUPPORTED_ERROR};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::media::{Media, ViewerItem};
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

const UPLOAD_PROGRESS_DELAY: Duration = Duration::from_millis(150);

/// One staged attachment and what its tile needs.
pub(super) struct Staged {
    pub(super) attachment: Attachment,
    preview: Option<Arc<Image>>,
    /// The bytes, kept until the upload lands so a failed one can retry.
    bytes: Option<Arc<Vec<u8>>>,
    /// Upload progress in `[0.0, 1.0]` and whether the 150ms delay has elapsed.
    progress: f64,
    show_progress: bool,
    upload_delay: Option<Task<()>>,
    /// Set when a persisted file from a restored draft no longer exists on the server.
    pub(super) needs_reattach: bool,
    reattach_reason: Option<String>,
    /// Retained for `copy_attachment_owned` when pasting copied chips without bytes, or snapshot fixtures.
    _copy_upload: Option<Task<()>>,
    settled: Vec<oneshot::Sender<()>>,
    source: Option<String>,
    /// Optional SnapShot source metadata (`pingdotgg/t3code#37771`).
    pub(super) snapshot_source: Option<serde_json::Value>,
}

impl Staged {
    fn ready(attachment: Attachment) -> Self {
        Self {
            attachment,
            preview: None,
            bytes: None,
            progress: 1.0,
            show_progress: false,
            upload_delay: None,
            needs_reattach: false,
            reattach_reason: None,
            _copy_upload: None,
            settled: Vec::new(),
            source: None,
            snapshot_source: None,
        }
    }

    fn matches_reattach(&self, name: &str, mime_type: &str, size_bytes: u64) -> bool {
        self.needs_reattach
            && self.attachment.kind == AttachmentKind::File
            && self.attachment.name == name
            && self.attachment.mime_type == mime_type
            && self.attachment.size_bytes == size_bytes
    }
}

#[derive(Default)]
pub(super) struct Attachments {
    pub(super) staged: HashMap<Target, Vec<Staged>>,
    attempts: rules::UploadAttempts,
    pub(super) preparations: t3_core::requests::attachments::Preparations<Vec<Result<Picked, String>>>,
    pub(super) grant_watch: Option<(u64, Task<()>)>,
    pub(super) upload_cursor: u64,
    #[cfg(feature = "snapshot")]
    fixture_applied: bool,
}

#[cfg(feature = "snapshot")]
#[derive(Clone)]
pub(crate) struct Fixture(pub serde_json::Value);
#[cfg(feature = "snapshot")]
impl gpui::Global for Fixture {}

/// A file read and prepared (HEIC converted / image downscaled) for staging.
pub(super) struct Picked {
    name: String,
    mime_type: String,
    kind: AttachmentKind,
    pub(super) bytes: Vec<u8>,
}

fn job_key(target: &Target, id: &str) -> String {
    format!("{}::{id}", super::drafts::key(target))
}

fn draft_target(target: &Target, id: &str) -> DraftTarget {
    DraftTarget {
        draft_id: super::drafts::key(target),
        file_id: id.to_owned(),
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).filter(|name| !name.is_empty()).unwrap_or_else(|| "file".to_owned())
}

fn image_format(mime_type: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(mime_type)
}

#[cfg(target_os = "macos")]
fn convert_heic_bytes(name: &str, bytes: &[u8]) -> Result<(String, String, Vec<u8>), String> {
    let dir = std::env::temp_dir().join(format!("t3-heic-{}", uuid_v4()));
    if std::fs::create_dir_all(&dir).is_err() {
        return Err(rules::unsupported_image_message(name));
    }
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("heic");
    let src = dir.join(format!("input.{ext}"));
    let dst = dir.join("output.png");
    let result = (|| {
        std::fs::write(&src, bytes).map_err(|_| rules::unsupported_image_message(name))?;
        let status = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "png"])
            .arg(&src)
            .args(["--out"])
            .arg(&dst)
            .output()
            .map_err(|_| rules::unsupported_image_message(name))?;
        if !status.status.success() {
            return Err(rules::unsupported_image_message(name));
        }
        let converted = std::fs::read(&dst).map_err(|_| rules::unsupported_image_message(name))?;
        let out_name = image_rules::file_name_for_compressed_mime(name, "image/png");
        Ok((out_name, "image/png".to_owned(), converted))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(not(target_os = "macos"))]
fn convert_heic_bytes(name: &str, _bytes: &[u8]) -> Result<(String, String, Vec<u8>), String> {
    Err(rules::unsupported_image_message(name))
}

fn encode_resized_image(
    img: &::image::DynamicImage,
    mime_type: &str,
    quality: f32,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match mime_type {
        "image/png" => {
            img.write_to(&mut std::io::Cursor::new(&mut out), ::image::ImageFormat::Png).ok()?;
        }
        "image/webp" => {
            img.write_to(&mut std::io::Cursor::new(&mut out), ::image::ImageFormat::WebP).ok()?;
        }
        _ => {
            let q = (quality * 100.0).round().clamp(1.0, 100.0) as u8;
            let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
            img.to_rgb8().write_with_encoder(encoder).ok()?;
        }
    }
    Some(out)
}

pub(super) fn prepare_image_bytes(
    mut name: String,
    mut mime_type: String,
    mut bytes: Vec<u8>,
    convertible_heic: bool,
) -> Result<Picked, String> {
    if convertible_heic {
        let validation = image_rules::validate_heic_image_dimensions(&bytes);
        if let Some(err) = image_rules::heic_validation_error_message(&name, validation) {
            return Err(err);
        }
        if !image_rules::is_compressible_source_size(bytes.len() as u64) {
            return Err(rules::too_large_message(&name, rules::MAX_IMAGE_BYTES));
        }
        (name, mime_type, bytes) = convert_heic_bytes(&name, &bytes)?;
    }

    let size_bytes = bytes.len() as u64;
    if image_rules::is_animated_gif_passthrough(&mime_type, size_bytes, rules::MAX_IMAGE_BYTES) {
        return Ok(Picked { name, mime_type, kind: AttachmentKind::Image, bytes });
    }
    let dims = image_rules::read_image_dimensions(&bytes);
    if !image_rules::needs_image_compression(&mime_type, size_bytes, rules::MAX_IMAGE_BYTES, dims) {
        if size_bytes > rules::MAX_IMAGE_BYTES {
            return Err(rules::too_large_message(&name, rules::MAX_IMAGE_BYTES));
        }
        return Ok(Picked { name, mime_type, kind: AttachmentKind::Image, bytes });
    }
    if !image_rules::is_compressible_source_size(size_bytes) {
        return Err(rules::too_large_message(&name, rules::MAX_IMAGE_BYTES));
    }

    let Ok(decoded) = ::image::load_from_memory(&bytes) else {
        return if size_bytes > rules::MAX_IMAGE_BYTES {
            Err(rules::too_large_message(&name, rules::MAX_IMAGE_BYTES))
        } else {
            Ok(Picked { name, mime_type, kind: AttachmentKind::Image, bytes })
        };
    };

    let (fit_w, fit_h) = image_rules::fit_image_to_max_edge(decoded.width(), decoded.height());
    let base = if fit_w != decoded.width() || fit_h != decoded.height() {
        decoded.resize_exact(fit_w, fit_h, ::image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };

    let primary_mime = image_rules::compression_target_mime(&mime_type, false);
    if primary_mime == "image/png"
        && let Some(encoded) = encode_resized_image(&base, "image/png", 1.0)
        && encoded.len() as u64 <= rules::MAX_IMAGE_BYTES
    {
        return Ok(Picked {
            name: image_rules::file_name_for_compressed_mime(&name, "image/png"),
            mime_type: "image/png".to_owned(),
            kind: AttachmentKind::Image,
            bytes: encoded,
        });
    }

    let fallback_mime = image_rules::compression_target_mime(&mime_type, true);
    let scale_steps: [f32; 3] = [1.0, image_rules::FALLBACK_SCALE_STEPS[0], image_rules::FALLBACK_SCALE_STEPS[1]];
    let mut best: Option<Vec<u8>> = None;
    for scale in scale_steps {
        let candidate = if (scale - 1.0).abs() < f32::EPSILON {
            None
        } else {
            let sw = ((base.width() as f32) * scale).round().max(1.0) as u32;
            let sh = ((base.height() as f32) * scale).round().max(1.0) as u32;
            Some(base.resize_exact(sw, sh, ::image::imageops::FilterType::Lanczos3))
        };
        let target_img = candidate.as_ref().unwrap_or(&base);
        for quality in image_rules::QUALITY_STEPS {
            if let Some(encoded) = encode_resized_image(target_img, fallback_mime, quality) {
                if encoded.len() as u64 <= rules::MAX_IMAGE_BYTES {
                    return Ok(Picked {
                        name: image_rules::file_name_for_compressed_mime(&name, fallback_mime),
                        mime_type: fallback_mime.to_owned(),
                        kind: AttachmentKind::Image,
                        bytes: encoded,
                    });
                }
                if best.as_ref().is_none_or(|prev| encoded.len() < prev.len()) {
                    best = Some(encoded);
                }
            }
        }
    }

    if let Some(encoded) = best
        && encoded.len() as u64 <= rules::MAX_IMAGE_BYTES
    {
        return Ok(Picked {
            name: image_rules::file_name_for_compressed_mime(&name, fallback_mime),
            mime_type: fallback_mime.to_owned(),
            kind: AttachmentKind::Image,
            bytes: encoded,
        });
    }
    Err(rules::too_large_message(&name, rules::MAX_IMAGE_BYTES))
}

fn prepare_picked_file(path: &std::path::Path, file_limit: Option<u64>) -> Result<Picked, String> {
    let name = file_name(path);
    let bytes = std::fs::read(path).map_err(|_| format!("'{name}' is empty or could not be read."))?;
    if bytes.is_empty() {
        return Err(format!("'{name}' is empty or could not be read."));
    }
    match image_rules::classify_file_with_mime(&name, None) {
        image_rules::MimeClassified::UnsupportedImage => Err(rules::unsupported_image_message(&name)),
        image_rules::MimeClassified::Image { mime_type, convertible_heic } => {
            prepare_image_bytes(name, mime_type, bytes, convertible_heic)
        }
        image_rules::MimeClassified::File { mime_type } => {
            let Some(limit) = file_limit else {
                return Err("This server does not support file attachments.".to_owned());
            };
            if bytes.len() as u64 > limit {
                return Err(rules::too_large_message(&name, limit));
            }
            Ok(Picked { name, mime_type, kind: AttachmentKind::File, bytes })
        }
    }
}

impl Composer {
    /// The open target's attachments; none while a queued message is open for editing.
    pub(super) fn staged(&self) -> &[Staged] {
        if self.editing_queued().is_some() {
            return &[];
        }
        self.attachment_target().as_ref().map(|target| self.staged_for(target)).unwrap_or_default()
    }

    pub(super) fn staged_for(&self, target: &Target) -> &[Staged] {
        self.attachments.staged.get(target).map(Vec::as_slice).unwrap_or_default()
    }

    pub(super) fn has_pending_attachments(&self, target: &Target) -> bool {
        self.attachments.preparations.count(&super::drafts::key(target)) > 0
            || self.staged_for(target).iter().any(|entry| matches!(entry.attachment.upload, Upload::Uploading) && !entry.needs_reattach)
    }

    pub(super) fn release_target_uploads(&self, target: &Target, cx: &App) {
        self.state.read(cx).uploads.release_draft(&super::drafts::key(target));
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn begin_test_preparation(&mut self, key: &str) {
        self.attachments.preparations.begin(key);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn seed_test_attachments(&mut self, thread_id: &str, count: usize) {
        let target = Target::Thread(thread_id.to_owned());
        let attachments = (0..count)
            .map(|i| Staged::ready(Attachment {
                id: format!("att-{i}"),
                kind: AttachmentKind::File,
                name: format!("file-{i}.txt"),
                mime_type: "text/plain".into(),
                size_bytes: 1,
                pasted_text: false,
                upload: Upload::Ready { attachment_id: format!("uploaded-{i}") },
            }));
        self.attachments.staged.entry(target).or_default().extend(attachments);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn stage_picked_for_test(
        &mut self,
        target: &Target,
        name: String,
        mime_type: String,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        self.stage_picked(
            target,
            Picked {
                name,
                mime_type,
                kind: AttachmentKind::File,
                bytes,
            },
            false,
            cx,
        );
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn seed_queue_ready_for_test(&self, target: &Target, id: &str, attachment_id: &str, cx: &App) {
        let client = self.state.read(cx).client_handle().unwrap();
        let env_id = client.environment().to_owned();
        let key = job_key(target, id);
        self.state.read(cx).uploads.start_upload(
            key,
            draft_target(target, id),
            client,
            UploadPayload {
                request: serde_json::json!({"name":"spec.md","mimeType":"text/markdown","sizeBytes":64}),
                mime_type: "text/markdown".into(),
                bytes: None,
                persisted_attachment_id: None,
                current_state: Some(AttachmentUploadState::Failed {
                    environment_id: "other-env".into(),
                    reason: "interrupted".into(),
                    attachment_id: None,
                    previous: Some(t3_core::composer::attachments::queue::ReadyAttachmentUpload {
                        environment_id: env_id,
                        attachment_id: attachment_id.to_owned(),
                    }),
                }),
            },
        );
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(super) fn find_staged_upload_ready_sync_test(
        &mut self,
        target: &Target,
        id: &str,
        attachment_id: &str,
        cx: &mut Context<Self>,
    ) {
        let mut uploading = Staged::ready(Attachment {
            id: id.to_owned(),
            kind: AttachmentKind::File,
            name: "sync.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 4,
            pasted_text: false,
            upload: Upload::Uploading,
        });
        uploading.bytes = Some(Arc::new(b"sync".to_vec()));
        self.attachments.staged.entry(target.clone()).or_default().push(uploading);
        let client = self.state.read(cx).client_handle().unwrap();
        let env_id = client.environment().to_owned();
        let key = job_key(target, id);
        self.state.read(cx).uploads.start_upload(
            key,
            draft_target(target, id),
            client,
            UploadPayload {
                request: serde_json::json!({"name":"sync.txt","mimeType":"text/plain","sizeBytes":4}),
                mime_type: "text/plain".into(),
                bytes: None,
                persisted_attachment_id: None,
                current_state: Some(AttachmentUploadState::Failed {
                    environment_id: "other-env".into(),
                    reason: "interrupted".into(),
                    attachment_id: None,
                    previous: Some(t3_core::composer::attachments::queue::ReadyAttachmentUpload {
                        environment_id: env_id,
                        attachment_id: attachment_id.to_owned(),
                    }),
                }),
            },
        );
        self.sync_upload_states_for(target, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn test_staged_thread_attachments(&self, thread_id: &str) -> Vec<crate::app_state::RestoredAttachment> {
        self.staged_for(&Target::Thread(thread_id.to_owned()))
            .iter()
            .map(|entry| crate::app_state::RestoredAttachment {
                kind: entry.attachment.kind,
                name: entry.attachment.name.clone(),
                mime_type: entry.attachment.mime_type.clone(),
                bytes: entry.bytes.as_deref().cloned().unwrap_or_default(),
            })
            .collect()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn test_staged_draft_attachments(&self, project_id: &str) -> Vec<crate::app_state::RestoredAttachment> {
        self.staged_for(&Target::Draft(project_id.to_owned()))
            .iter()
            .map(|entry| crate::app_state::RestoredAttachment {
                kind: entry.attachment.kind,
                name: entry.attachment.name.clone(),
                mime_type: entry.attachment.mime_type.clone(),
                bytes: entry.bytes.as_deref().cloned().unwrap_or_default(),
            })
            .collect()
    }

    fn cancel_pending_routed_send_for(&mut self, target: &Target) -> bool {
        let matches = self.snapshot.target.as_ref() == Some(target)
            || self
                .pending_routed_send
                .as_ref()
                .is_some_and(|pending| *target == Target::Draft(pending.project_id.clone()));
        matches && self.cancel_pending_routed_send()
    }

    /// Attachments a saved draft restores: uploaded, without their bytes or preview.
    pub(super) fn seed_attachments(&mut self, target: &Target, restored: Vec<Attachment>, verify: bool, cx: &mut Context<Self>) {
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        if restored.is_empty() {
            return;
        }
        if self.cancel_pending_routed_send_for(target) {
            cx.notify();
        }
        let ids: Vec<String> = if verify {
            restored.iter().filter_map(|a| a.attachment_id().map(|_| a.id.clone())).collect()
        } else {
            Vec::new()
        };
        let staged = restored.into_iter().map(Staged::ready);
        self.attachments.staged.entry(target.clone()).or_default().extend(staged);
        for id in ids {
            self.start_upload(target.clone(), id, cx);
        }
    }

    pub(super) fn stage_imported_attachment(&mut self, target: &Target, imported: super::imports::ImportedAttachment, cx: &mut Context<Self>) {
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        if self.cancel_pending_routed_send_for(target) { cx.notify(); }
        let id = imported.attachment.id.clone();
        let copying = imported.source.is_some();
        let mut entry = Staged::ready(imported.attachment);
        entry.source = imported.source;
        entry.snapshot_source = imported.snapshot_source;
        self.attachments.staged.entry(target.clone()).or_default().push(entry);
        if copying { self.start_upload(target.clone(), id, cx); }
    }

    pub(super) fn staged_attachments(&self) -> Vec<Attachment> {
        self.staged().iter().map(|staged| staged.attachment.clone()).collect()
    }

    /// `fileAttachmentStagingLimit`: the largest file the server takes, `None` for no files.
    fn file_limit(&self, cx: &App) -> Option<u64> {
        let config = self.state.read(cx).server_config.config.as_ref()?;
        rules::file_staging_limit(config.environment.capabilities.file_attachments.map(|files| files.max_upload_bytes))
    }

    pub(super) fn attachment_target(&self) -> Option<Target> {
        if let Some((request, question)) = self.active_question() {
            return self.question_target(&request, &question.id);
        }
        self.snapshot.target.clone()
    }

    /// Draft key of the currently open question attachment target, if any, for SnapShot pinning.
    pub(crate) fn active_question_draft_key(&self) -> Option<String> {
        let (request, question) = self.active_question()?;
        let target = self.question_target(&request, &question.id)?;
        Some(super::drafts::key(&target))
    }

    /// Delivers a captured SnapShot PNG + optional `SnapShotSource` metadata into the active
    /// thread/draft (or a pinned question attachment draft), enforcing
    /// `PROVIDER_SEND_TURN_MAX_ATTACHMENTS` and `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES`.
    pub(crate) fn attach_snapshot_png(
        &mut self,
        capture_id: &str,
        name: String,
        png_bytes: Vec<u8>,
        snapshot_source: Option<serde_json::Value>,
        pinned_question_draft: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.editing_queued().is_some() || self.is_rewinding(cx) {
            return Err("Finish editing before attaching a snapshot.".to_owned());
        }
        let target = if let Some(pinned) = pinned_question_draft
            && let Some((request, question)) = self.active_question()
            && let Some(q_target) = self.question_target(&request, &question.id)
            && super::drafts::key(&q_target) == pinned
        {
            if let Target::Question(ref key) = q_target
                && !self.question_can_attach(key, cx, IntakePhase::Start)
            {
                return Err(QUESTION_ATTACHMENTS_UNSUPPORTED_ERROR.to_owned());
            }
            q_target
        } else {
            // Read live selection (`self.fixed` -> `state.selected_thread` -> `state.draft_project`)
            // before falling back to `self.snapshot.target`, matching `Composer::read_snapshot`, so
            // synchronous route switches from `SnapShotCoordinator::resolve_or_create_target` never
            // stage onto a stale target before GPUI flushes `Composer::refresh`.
            let state = self.state.read(cx);
            match (
                self.fixed.as_ref().or(state.selected_thread.as_ref()),
                &state.draft_project,
            ) {
                (Some(thread_id), _) => Target::Thread(thread_id.clone()),
                (None, Some(project_id)) => Target::Draft(project_id.clone()),
                _ => match self.snapshot.target.clone() {
                    Some(target) => target,
                    None => return Err("No active thread or draft.".to_owned()),
                },
            }
        };

        if matches!(target, Target::Thread(ref id) if self.state.read(cx).is_thread_rewinding(id)) {
            return Err("This thread is rewinding. Try the capture again.".to_owned());
        }

        // Idempotent by capture ID (`SnapShotCoordinator.tsx`).
        if self.staged_for(&target).iter().any(|entry| entry.attachment.id == capture_id) {
            return Ok(());
        }

        if self.reserved_attachment_count(&target) >= rules::MAX_ATTACHMENTS {
            return Err("Remove an attachment, then try this capture again.".to_owned());
        }

        let size = png_bytes.len() as u64;
        if size == 0 || size > rules::MAX_IMAGE_BYTES {
            return Err("The captured window is too large to attach.".to_owned());
        }

        if self.attachment_target().as_ref() == Some(&target) {
            self.cancel_pending_send();
        }
        self.cancel_pending_routed_send_for(&target);

        let preview = Some(Arc::new(Image::from_bytes(ImageFormat::Png, png_bytes.clone())));
        let attachment = Attachment {
            id: capture_id.to_owned(),
            kind: AttachmentKind::Image,
            name,
            mime_type: "image/png".to_owned(),
            size_bytes: size,
            pasted_text: false,
            upload: Upload::Uploading,
        };
        let id = attachment.id.clone();
        self.attachments.staged.entry(target.clone()).or_default().push(Staged {
            attachment,
            preview,
            bytes: Some(Arc::new(png_bytes)),
            progress: 0.0,
            show_progress: false,
            upload_delay: None,
            needs_reattach: false,
            reattach_reason: None,
            _copy_upload: None,
            settled: Vec::new(),
            source: None,
            snapshot_source,
        });
        self.record_draft(&target, cx);
        self.start_upload(target, id, cx);
        cx.notify();
        Ok(())
    }

    pub(crate) fn can_attach(&self, cx: &App) -> bool {
        if self.editing_queued().is_some() || self.is_rewinding(cx) { return false; }
        if let Some(Target::Question(key)) = self.attachment_target() { return self.question_can_attach(&key, cx, IntakePhase::Start); }
        self.snapshot.target.is_some() && self.snapshot.pending.user_inputs.is_empty()
    }

    /// `showComposerAttachAction`.
    pub(super) fn shows_attach_button(&self, cx: &App) -> bool {
        self.can_attach(cx) && self.file_limit(cx).is_some()
    }

    /// The paperclip: a file dialog for any number of files.
    pub(super) fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach(cx) { return; }
        let target = self.attachment_target();
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.attachment_target() != target { return; }
                this.attach_paths(paths, window, cx);
                this.focus(window, cx);
            });
        })
        .detach();
    }

    /// Files dropped, pasted from Finder, or picked. Folders become mentions when the server
    /// runs on this machine, as the web's `addComposerFolders` allows.
    pub(super) fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach(cx) {
            if self.active_question().is_some() {
                self.error = Some(QUESTION_ATTACHMENTS_UNSUPPORTED_ERROR.to_owned());
                cx.notify();
            }
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
        let Some(target) = self.attachment_target() else { return };
        let storage_key = super::drafts::key(&target);
        let preparation = self.attachments.preparations.begin(&storage_key);
        let file_limit = self.file_limit(cx);
        self.cancel_pending_send();
        self.cancel_pending_routed_send();
        cx.notify();
        let read = cx.background_executor().spawn(async move {
            files
                .into_iter()
                .map(|path| prepare_picked_file(&path, file_limit))
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.attachments.preparations.complete(&storage_key, preparation, picked) { return; }
                this.finish_prepared_reads(cx);
                this.try_flush_pending_submit_after_prepare(&storage_key, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn finish_prepared_reads(&mut self, cx: &mut Context<Self>) -> bool {
        let keys: Vec<_> = self.attachments.preparations.keys().cloned().collect();
        let mut changed = false;
        for key in keys {
            let Some(target) = super::drafts::target(&key) else { continue };
            let eligible = match &target {
                Target::Question(question) => self.question_can_attach(question, cx, IntakePhase::Preparing),
                Target::Thread(id) => !self.state.read(cx).is_thread_rewinding(id),
                _ => true,
            };
            for picked in self.attachments.preparations.take_completed(&key, eligible) {
                changed = true;
                for file in picked {
                    match file {
                        Ok(file) => self.stage_picked(&target, file, false, cx),
                        Err(error) if self.attachment_target().as_ref() == Some(&target) => self.error = Some(error),
                        Err(_) => {},
                    }
                }
            }
        }
        changed
    }

    fn stage_picked(&mut self, target: &Target, picked: Picked, pasted_text: bool, cx: &mut Context<Self>) {
        if let Target::Question(key) = target && !self.question_can_attach(key, cx, IntakePhase::Preparing) { return; }
        if matches!(target, Target::Thread(id) if self.state.read(cx).is_thread_rewinding(id)) { return; }
        if self.attachment_target().as_ref() == Some(target) { self.cancel_pending_send(); }
        if self.cancel_pending_routed_send_for(target) { cx.notify(); }
        let size = picked.bytes.len() as u64;
        // Distinct images with matching metadata must both stage, while byte-identical repeats dedup (`pingdotgg/t3code#37116`).
        if picked.kind == AttachmentKind::Image {
            let incoming_key = image_rules::composer_image_dedup_key_with_bytes(
                &picked.mime_type,
                size,
                &picked.name,
                Some(&picked.bytes),
            );
            let duplicate = self.staged_for(target).iter().any(|existing| {
                existing.attachment.kind == AttachmentKind::Image
                    && existing.bytes.as_deref().is_some_and(|bytes| {
                        image_rules::composer_image_dedup_key_with_bytes(
                            &existing.attachment.mime_type,
                            existing.attachment.size_bytes,
                            &existing.attachment.name,
                            Some(bytes),
                        ) == incoming_key
                    })
            });
            if duplicate {
                return;
            }
        }
        // Replacing a matching reattach marker must not consume an extra slot (`useComposerAttachmentIntake.ts`).
        if picked.kind == AttachmentKind::File
            && let Some(reattach_id) = self.staged_for(target).iter().find(|s| s.matches_reattach(&picked.name, &picked.mime_type, size)).map(|s| s.attachment.id.clone())
        {
            if let Some(staged) = self.find_staged(target, &reattach_id) {
                staged.bytes = Some(Arc::new(picked.bytes));
                staged.needs_reattach = false;
                staged.reattach_reason = None;
                staged.attachment.upload = Upload::Uploading;
                staged.progress = 0.0;
                staged.show_progress = false;
            }
            self.record_draft(target, cx);
            self.start_upload(target.clone(), reattach_id, cx);
            return;
        }
        let staged = self.reserved_attachment_count(target);
        let classified = match picked.kind {
            AttachmentKind::Image => rules::Classified::Image { mime_type: "image/png" },
            AttachmentKind::File => rules::Classified::File { mime_type: picked.mime_type.clone() },
        };
        if let Some(error) = rules::admission_error(staged, &picked.name, &classified, size, self.file_limit(cx)) {
            if self.attachment_target().as_ref() == Some(target) { self.error = Some(error); }
            return;
        }
        self.push_staged_entry(target, picked.kind, picked.name, picked.mime_type, picked.bytes, pasted_text, cx);
    }

    /// Admits raw text or bytes (`fold_paste` / `restore_reverted_attachments`) and starts upload.
    fn stage(&mut self, target: &Target, name: String, bytes: Vec<u8>, pasted_text: bool, cx: &mut Context<Self>) {
        if pasted_text {
            self.stage_picked(target, Picked {
                name,
                mime_type: "text/plain;charset=utf-8".to_owned(),
                kind: AttachmentKind::File,
                bytes,
            }, true, cx);
            return;
        }
        match image_rules::classify_file_with_mime(&name, None) {
            image_rules::MimeClassified::UnsupportedImage => {
                if self.attachment_target().as_ref() == Some(target) {
                    self.error = Some(rules::unsupported_image_message(&name));
                }
            }
            image_rules::MimeClassified::Image { mime_type, convertible_heic } => {
                match prepare_image_bytes(name, mime_type, bytes, convertible_heic) {
                    Ok(picked) => self.stage_picked(target, picked, false, cx),
                    Err(error) if self.attachment_target().as_ref() == Some(target) => self.error = Some(error),
                    Err(_) => {}
                }
            }
            image_rules::MimeClassified::File { mime_type } => {
                self.stage_picked(target, Picked { name, mime_type, kind: AttachmentKind::File, bytes }, false, cx);
            }
        }
    }

    pub(super) fn restore_reverted_attachments(&mut self, thread_id: &str, attachments: Vec<crate::app_state::RestoredAttachment>, cx: &mut Context<Self>) {
        if attachments.is_empty() { return; }
        let target = Target::Thread(thread_id.to_owned());
        if self.attachment_target().as_ref() == Some(&target) { self.cancel_pending_send(); }
        for restored in attachments {
            if self.reserved_attachment_count(&target) >= rules::MAX_ATTACHMENTS { break; }
            self.push_staged_entry(&target, restored.kind, restored.name, restored.mime_type, restored.bytes, false, cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_staged_entry(
        &mut self,
        target: &Target,
        kind: AttachmentKind,
        name: String,
        mime_type: String,
        bytes: Vec<u8>,
        pasted_text: bool,
        cx: &mut Context<Self>,
    ) {
        let size = bytes.len() as u64;
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
            progress: 0.0,
            show_progress: false,
            upload_delay: None,
            needs_reattach: false,
            reattach_reason: None,
            _copy_upload: None,
            settled: Vec::new(),
            source: None,
            snapshot_source: None,
        });
        self.record_draft(target, cx);
        self.start_upload(target.clone(), id, cx);
    }

    fn find_staged(&mut self, target: &Target, id: &str) -> Option<&mut Staged> {
        self.attachments.staged.get_mut(target)?.iter_mut().find(|staged| staged.attachment.id == id)
    }

    fn schedule_upload_progress_delay(&mut self, target: &Target, id: &str, cx: &mut Context<Self>) {
        let task_target = target.clone();
        let task_id = id.to_owned();
        let delay = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UPLOAD_PROGRESS_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                let Some(staged) = this.find_staged(&task_target, &task_id) else { return };
                if staged.attachment.upload == Upload::Uploading && !staged.show_progress {
                    staged.show_progress = true;
                    cx.notify();
                }
            });
        });
        if let Some(staged) = self.find_staged(target, id) {
            staged.upload_delay = Some(delay);
        }
    }

    /// Mints an upload and dispatches it through `AttachmentUploadQueueClient` (or `copy_attachment_owned`
    /// for clipboard-copied chips that reference a server attachment ID without local bytes).
    pub(super) fn start_upload(&mut self, target: Target, id: String, cx: &mut Context<Self>) {
        let (client, uploads, runtime) = {
            let state = self.state.read(cx);
            (
                state.client_handle(),
                state.uploads.clone(),
                state.runtime().clone(),
            )
        };
        let Some(staged) = self.find_staged(&target, &id) else { return };
        let source = staged.source.clone();
        let bytes = staged.bytes.as_deref().cloned();
        let persisted_id = staged.attachment.attachment_id().map(str::to_owned);
        if source.is_none() && bytes.is_none() && persisted_id.is_none() {
            return;
        }
        let Some(client) = client else {
            if bytes.is_some() || source.is_some() {
                staged.attachment.upload = Upload::Failed { reason: "Not connected".to_owned() };
                staged.show_progress = false;
                staged.upload_delay = None;
            }
            return;
        };

        if let Some(source) = source {
            self.attachments.attempts.cancel(&id);
            let Some(staged) = self.find_staged(&target, &id) else { return };
            staged._copy_upload = None;
            staged.settled.clear();
            staged.attachment.upload = Upload::Uploading;
            staged.progress = 0.0;
            staged.show_progress = false;
            let attachment = staged.attachment.clone();
            let attempt = self.attachments.attempts.begin(&id);
            let (task_target, task_id) = (target.clone(), id.clone());
            let task = cx.spawn(async move |this, cx| {
                let result = runtime
                    .spawn(async move {
                        t3_client::upload::copy_attachment_owned(client, &attachment, &source).await
                    })
                    .await
                    .unwrap_or_else(|_| Err("Upload failed".to_owned()));
                let _ = this.update(cx, |this, cx| {
                    if !this.attachments.attempts.finish(&task_id, attempt) { return; }
                    let Some(staged) = this.find_staged(&task_target, &task_id) else { return };
                    staged.settled.clear();
                    staged.upload_delay = None;
                    staged.show_progress = false;
                    match result {
                        Ok(attachment_id) => {
                            staged.attachment.upload = Upload::Ready { attachment_id: attachment_id.into_id() };
                            staged.bytes = None;
                            staged.progress = 1.0;
                            this.record_draft(&task_target, cx);
                        }
                        Err(reason) => staged.attachment.upload = Upload::Failed { reason },
                    }
                    cx.notify();
                });
            });
            if let Some(staged) = self.find_staged(&target, &id) {
                staged._copy_upload = Some(task);
            }
            self.schedule_upload_progress_delay(&target, &id, cx);
            return;
        }

        let payload = UploadPayload {
            request: staged.attachment.upload_request(),
            mime_type: staged.attachment.mime_type.clone(),
            bytes,
            persisted_attachment_id: persisted_id.clone(),
            current_state: None,
        };
        if persisted_id.is_none() {
            staged.attachment.upload = Upload::Uploading;
            staged.progress = 0.0;
            staged.show_progress = false;
            staged.needs_reattach = false;
            staged.reattach_reason = None;
        }
        let key = job_key(&target, &id);
        let d_target = draft_target(&target, &id);
        let enqueued = uploads.start_upload(key, d_target, client, payload).is_some();
        if enqueued && persisted_id.is_none() {
            self.schedule_upload_progress_delay(&target, &id, cx);
        }
    }

    pub(super) fn apply_upload_events(&mut self, cx: &mut Context<Self>) {
        let events = self.state.read(cx).upload_events_since(&mut self.attachments.upload_cursor);
        if events.is_empty() {
            return;
        }
        let mut changed = false;
        let mut drafts_to_record: Vec<Target> = Vec::new();
        for event in events {
            match event {
                QueueEvent::Progress { target, state, .. } => {
                    let Some(composer_target) = super::drafts::target(&target.draft_id) else { continue };
                    let Some(staged) = self.find_staged(&composer_target, &target.file_id) else { continue };
                    if let AttachmentUploadState::Uploading { progress, .. } = state {
                        staged.attachment.upload = Upload::Uploading;
                        staged.progress = progress;
                        staged.needs_reattach = false;
                        staged.reattach_reason = None;
                        changed = true;
                    }
                }
                QueueEvent::StateChanged { target, state, .. } => {
                    let Some(composer_target) = super::drafts::target(&target.draft_id) else { continue };
                    let Some(staged) = self.find_staged(&composer_target, &target.file_id) else { continue };
                    match state {
                        Some(AttachmentUploadState::Uploading { progress, .. }) => {
                            let was_uploading = staged.attachment.upload == Upload::Uploading;
                            staged.attachment.upload = Upload::Uploading;
                            staged.progress = progress;
                            staged.needs_reattach = false;
                            staged.reattach_reason = None;
                            if !was_uploading {
                                staged.show_progress = false;
                                self.schedule_upload_progress_delay(&composer_target, &target.file_id, cx);
                            }
                            changed = true;
                        }
                        Some(AttachmentUploadState::Ready(ready)) => {
                            staged.attachment.upload = Upload::Ready { attachment_id: ready.attachment_id };
                            staged.progress = 1.0;
                            staged.show_progress = false;
                            staged.upload_delay = None;
                            staged.needs_reattach = false;
                            staged.reattach_reason = None;
                            staged.settled.clear();
                            if !drafts_to_record.contains(&composer_target) {
                                drafts_to_record.push(composer_target);
                            }
                            changed = true;
                        }
                        Some(AttachmentUploadState::Failed { reason, .. }) => {
                            staged.attachment.upload = Upload::Failed { reason };
                            staged.progress = 0.0;
                            staged.show_progress = false;
                            staged.upload_delay = None;
                            staged.settled.clear();
                            changed = true;
                        }
                        None => {
                            staged.settled.clear();
                            changed = true;
                        }
                    }
                }
                QueueEvent::MissingPersistedFile { target, reason, .. } => {
                    let Some(composer_target) = super::drafts::target(&target.draft_id) else { continue };
                    let Some(staged) = self.find_staged(&composer_target, &target.file_id) else { continue };
                    staged.needs_reattach = true;
                    staged.reattach_reason = Some(reason.clone());
                    staged.attachment.upload = Upload::Failed { reason };
                    staged.bytes = None;
                    staged.progress = 0.0;
                    staged.show_progress = false;
                    staged.upload_delay = None;
                    staged.settled.clear();
                    changed = true;
                }
            }
        }
        for target in drafts_to_record {
            self.record_draft(&target, cx);
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn reconcile_upload_capabilities(&mut self, cx: &mut Context<Self>) {
        let config = self.state.read(cx).server_config.config.clone();
        let Some(config) = config else { return };
        let supports_files = config.environment.capabilities.file_attachments.is_some();
        let supports_questions = config.environment.capabilities.question_attachments == Some(true);
        let uploads = self.state.read(cx).uploads.clone();
        let targets: Vec<Target> = self.attachments.staged.keys().cloned().collect();
        let mut changed = false;
        for target in targets {
            let is_question = matches!(target, Target::Question(_));
            let lose_all = !supports_files || (is_question && !supports_questions);
            if !lose_all {
                continue;
            }
            let current: Vec<Attachment> = self.staged_for(&target).iter().map(|s| s.attachment.clone()).collect();
            let release_ids = image_rules::attachments_to_release_on_upload_capability_loss(&current);
            if release_ids.is_empty() {
                continue;
            }
            if let Some(entries) = self.attachments.staged.get_mut(&target) {
                entries.retain(|staged| {
                    if release_ids.contains(&staged.attachment.id) {
                        uploads.release_upload(&job_key(&target, &staged.attachment.id));
                        self.attachments.attempts.cancel(&staged.attachment.id);
                        false
                    } else {
                        true
                    }
                });
            }
            self.record_draft(&target, cx);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn sync_upload_states_for(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.apply_upload_events(cx);
        let uploads = self.state.read(cx).uploads.clone();
        let Some(entries) = self.attachments.staged.get_mut(target) else {
            return;
        };
        let mut became_ready = false;
        for staged in entries.iter_mut() {
            if staged.source.is_some() {
                continue;
            }
            let key = job_key(target, &staged.attachment.id);
            let Some(state) = uploads.state(&key) else {
                continue;
            };
            match state {
                AttachmentUploadState::Uploading { progress, .. } => {
                    staged.attachment.upload = Upload::Uploading;
                    staged.progress = progress;
                    staged.needs_reattach = false;
                    staged.reattach_reason = None;
                }
                AttachmentUploadState::Ready(ready) => {
                    if !matches!(&staged.attachment.upload, Upload::Ready { attachment_id } if *attachment_id == ready.attachment_id) {
                        became_ready = true;
                    }
                    staged.attachment.upload = Upload::Ready { attachment_id: ready.attachment_id };
                    staged.progress = 1.0;
                    staged.show_progress = false;
                    staged.upload_delay = None;
                    staged.needs_reattach = false;
                    staged.reattach_reason = None;
                    staged.settled.clear();
                }
                AttachmentUploadState::Failed { reason, .. } => {
                    staged.attachment.upload = Upload::Failed { reason };
                    staged.progress = 0.0;
                    staged.show_progress = false;
                    staged.upload_delay = None;
                    staged.settled.clear();
                }
            }
        }
        if became_ready {
            self.record_draft(target, cx);
        }
    }

    pub(super) fn retry_offline_staged_uploads(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.state.read(cx).phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected)) {
            return;
        }
        let uploads = self.state.read(cx).uploads.clone();
        let to_start: Vec<(Target, String)> = self
            .attachments
            .staged
            .iter()
            .flat_map(|(target, entries)| {
                entries.iter().filter_map(|staged| {
                    let needs_start = staged.source.is_none()
                        && staged.bytes.is_some()
                        && !staged.needs_reattach
                        && matches!(staged.attachment.upload, Upload::Failed { .. })
                        && uploads.state(&job_key(target, &staged.attachment.id)).is_none();
                    needs_start.then(|| (target.clone(), staged.attachment.id.clone()))
                })
            })
            .collect();
        if to_start.is_empty() {
            return;
        }
        for (target, id) in to_start {
            self.start_upload(target, id, cx);
        }
        cx.notify();
    }

    pub(super) fn target_job_keys(&self, target: &Target) -> Vec<String> {
        self.staged_for(target)
            .iter()
            .filter(|entry| entry.source.is_none())
            .map(|entry| job_key(target, &entry.attachment.id))
            .collect()
    }

    pub(super) fn retire_sent_attachments(&self, target: &Target, entries: &[Staged], cx: &App) {
        let keys: Vec<String> = entries
            .iter()
            .filter(|entry| entry.source.is_none())
            .map(|entry| job_key(target, &entry.attachment.id))
            .collect();
        self.state.read(cx).uploads.retire_sent(&keys);
    }

    /// Releases all queue jobs for `target` and deletes any ready server attachment IDs in
    /// `entries` that were NOT owned by `AttachmentUploadQueueClient` (which deletes its own).
    pub(super) fn release_discarded_attachments(&self, target: &Target, entries: &[Staged], cx: &App) {
        let uploads = &self.state.read(cx).uploads;
        let unowned_ids: Vec<String> = entries
            .iter()
            .filter(|entry| entry.source.is_some() || uploads.state(&job_key(target, &entry.attachment.id)).is_none())
            .filter_map(|entry| entry.attachment.attachment_id().map(str::to_owned))
            .collect();
        uploads.release_draft(&super::drafts::key(target));
        for id in unowned_ids {
            self.release_upload(&id, cx);
        }
    }

    pub(super) fn staged_needs_reattach(&self) -> bool {
        self.staged().iter().any(|entry| entry.needs_reattach)
    }

    pub(super) fn await_target_uploads(
        &mut self,
        target: &Target,
        retry_ids: &[String],
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<()>> {
        let uploads = self.state.read(cx).uploads.clone();
        let mut direct_start = Vec::new();
        let mut queue_retry_keys = Vec::new();
        for id in retry_ids {
            let Some(entry) = self.staged_for(target).iter().find(|s| s.attachment.id == *id) else {
                continue;
            };
            if entry.needs_reattach {
                continue;
            }
            let key = job_key(target, id);
            if entry.source.is_some() || uploads.state(&key).is_none() {
                direct_start.push(id.clone());
            } else {
                queue_retry_keys.push(key);
            }
        }
        for id in direct_start {
            self.start_upload(target.clone(), id, cx);
        }
        if !queue_retry_keys.is_empty() {
            uploads.retry_on_send(&queue_retry_keys);
        }
        let queue_wait_keys: Vec<String> = self
            .staged_for(target)
            .iter()
            .filter(|staged| staged.source.is_none() && staged.attachment.attachment_id().is_none() && !staged.needs_reattach)
            .map(|staged| job_key(target, &staged.attachment.id))
            .filter(|key| uploads.state(key).is_some())
            .collect();
        let copy_waiters = self.wait_for_uploads(target);
        if queue_wait_keys.is_empty() && copy_waiters.is_empty() {
            return None;
        }
        let runtime = self.state.read(cx).runtime().clone();
        Some(cx.background_executor().spawn(async move {
            let queue_fut = runtime.spawn(async move {
                if !queue_wait_keys.is_empty() {
                    uploads.await_uploads(&queue_wait_keys).await;
                }
            });
            let _ = futures::future::join(futures::future::join_all(copy_waiters), queue_fut).await;
        }))
    }

    pub(super) fn wait_for_uploads(&mut self, target: &Target) -> Vec<oneshot::Receiver<()>> {
        self.attachments.staged.get_mut(target).into_iter().flatten().filter(|staged| staged._copy_upload.is_some() && staged.attachment.upload == Upload::Uploading).map(|staged| {
            let (settled, receiver) = oneshot::channel();
            staged.settled.push(settled);
            receiver
        }).collect()
    }

    pub(super) fn release_upload(&self, attachment_id: &str, cx: &App) {
        let request = self.state.read(cx).rpc(rules::DELETE, rules::delete_request(attachment_id));
        cx.background_executor().spawn(async move { request.await.ok() }).detach();
    }

    pub(super) fn retry_upload(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.can_attach(cx) { return; }
        self.cancel_pending_send();
        self.cancel_pending_routed_send();
        let Some(target) = self.attachment_target() else { return };
        if self.staged_for(&target).iter().any(|s| s.attachment.id == id && s.source.is_none()) {
            let uploads = self.state.read(cx).uploads.clone();
            let key = job_key(&target, &id);
            if uploads.retry_upload(&key).is_some() {
                if let Some(staged) = self.find_staged(&target, &id) {
                    staged.attachment.upload = Upload::Uploading;
                    staged.progress = 0.0;
                    staged.show_progress = false;
                }
                self.schedule_upload_progress_delay(&target, &id, cx);
                cx.notify();
                return;
            }
        }
        self.start_upload(target, id, cx);
        cx.notify();
    }

    /// Removes a tile. An uploaded file the message never carried is deleted on the server.
    pub(super) fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || self.active_input().is_some_and(|input| self.question_responding(&input.request_id, cx)) { return; }
        let Some(target) = self.attachment_target() else { return };
        let Some(staged) = self.attachments.staged.get_mut(&target) else { return };
        let Some(index) = staged.iter().position(|staged| staged.attachment.id == id) else { return };
        let removed = staged.remove(index);
        self.attachments.attempts.cancel(id);
        let key = job_key(&target, id);
        let uploads = self.state.read(cx).uploads.clone();
        let owned_by_queue = removed.source.is_none() && uploads.state(&key).is_some();
        uploads.release_upload(&key);
        self.cancel_pending_send();
        self.cancel_pending_routed_send();
        if !owned_by_queue && let Some(attachment_id) = removed.attachment.attachment_id() {
            self.release_upload(attachment_id, cx);
        }
        self.record_draft(&target, cx);
        cx.notify();
    }

    /// Takes the target's attachments for a send; `restore_attachments` puts them back if it fails.
    pub(super) fn take_attachments(&mut self, target: &Target) -> Vec<Staged> {
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        self.attachments.staged.remove(target).unwrap_or_default()
    }

    pub(super) fn restore_attachments(&mut self, target: &Target, mut taken: Vec<Staged>) {
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        let current = self.attachments.staged.entry(target.clone()).or_default();
        taken.append(current);
        *current = taken;
    }

    /// Paste: images and copied files attach; a large text paste becomes a file. Returns whether
    /// the paste was handled, so plain text still reaches the prompt.
    pub(super) fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let target = self.attachment_target();
        if !matches!(target, Some(Target::Question(_)))
            && !self.is_rewinding(cx)
            && self.editing_queued().is_none()
            && let Some(text) = item.text()
        {
            let fragment = item.metadata().cloned().or_else(super::clipboard::read_html_fragment).unwrap_or_default();
            if self.paste_context(&text, &fragment, window, cx) {
                return true;
            }
        }
        if !self.can_attach(cx) {
            if self.active_question().is_some()
                && item.entries().iter().any(|entry| matches!(entry, ClipboardEntry::ExternalPaths(p) if !p.paths().is_empty()) || matches!(entry, ClipboardEntry::Image(_)))
            {
                self.error = Some(QUESTION_ATTACHMENTS_UNSUPPORTED_ERROR.to_owned());
                cx.notify();
                return true;
            }
            return false;
        }
        let Some(target) = target else { return false };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
                    self.attach_paths(paths.paths().to_vec(), window, cx);
                    return true;
                }
                ClipboardEntry::Image(image) => {
                    let format = image.format;
                    let Some(mime) = rules::image_mime_for_format(format.extension()) else {
                        self.error = Some(rules::unsupported_image_message(&format!("image.{}", format.extension())));
                        cx.notify();
                        return true;
                    };
                    let name = format!("image.{}", format.extension());
                    let bytes = image.bytes.clone();
                    let dims = image_rules::read_image_dimensions(&bytes);
                    if image_rules::needs_image_compression(mime, bytes.len() as u64, rules::MAX_IMAGE_BYTES, dims) {
                        let storage_key = super::drafts::key(&target);
                        let preparation = self.attachments.preparations.begin(&storage_key);
                        self.cancel_pending_send();
                        self.cancel_pending_routed_send();
                        cx.notify();
                        let mime_owned = mime.to_owned();
                        let task = cx.background_executor().spawn(async move {
                            vec![prepare_image_bytes(name, mime_owned, bytes, false)]
                        });
                        cx.spawn_in(window, async move |this, cx| {
                            let picked = task.await;
                            let _ = this.update_in(cx, |this, window, cx| {
                                if !this.attachments.preparations.complete(&storage_key, preparation, picked) { return; }
                                this.finish_prepared_reads(cx);
                                this.try_flush_pending_submit_after_prepare(&storage_key, window, cx);
                                cx.notify();
                            });
                        })
                        .detach();
                    } else {
                        self.stage(&target, name, bytes, false, cx);
                        cx.notify();
                    }
                    return true;
                }
                _ => {}
            }
        }
        let Some(text) = item.text() else { return false };
        if self.active_question().is_some_and(|(_, question)| question.initial_answer.is_some()) { return false; }
        self.fold_paste(&target, text, cx)
    }

    /// `foldPastedText`.
    fn fold_paste(&mut self, target: &Target, text: String, cx: &mut Context<Self>) -> bool {
        let selection = self.input.read(cx).selected_range();
        let exceeds = rules::paste_exceeds_limit(&self.prompt(cx), selection, &text);
        if !rules::folds_into_file(&text, false, exceeds) {
            return false;
        }
        let staged = self.reserved_attachment_count(target);
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
        if self.is_rewinding(cx) || self.active_question().is_some_and(|(request, question)| !question.allows_custom_answer() || self.question_responding(&request, cx)) { return; }
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

    fn open_staged_preview(&self, staged: &Staged, window: &mut Window, cx: &mut App) {
        if staged.needs_reattach {
            return;
        }
        if let Some(attachment_id) = staged.attachment.attachment_id() {
            let resource = t3_core::media::AssetResource::Attachment { attachment_id: attachment_id.to_owned() };
            if staged.attachment.kind == AttachmentKind::Image
                && let Some(viewer) = cx.try_global::<Media>().and_then(|media| media.scope.as_ref().and_then(|scope| scope.viewer.upgrade()))
            {
                let scope = cx.try_global::<Media>().and_then(|media| media.scope.clone());
                let item = ViewerItem::image(t3_core::media::MediaSource::Asset(resource), staged.attachment.name.clone());
                viewer.update(cx, |viewer, cx| viewer.open_with_scope(vec![item], 0, scope, window, cx));
                return;
            }
            crate::media::open_asset(&resource, cx);
            return;
        }
        if staged.attachment.kind == AttachmentKind::Image
            && let Some(bytes) = &staged.bytes
            && let Some(viewer) = cx.try_global::<Media>().and_then(|media| media.scope.as_ref().and_then(|scope| scope.viewer.upgrade()))
        {
            let data_url = image_rules::data_url(&staged.attachment.mime_type, bytes.as_slice());
            let item = ViewerItem::image(t3_core::media::MediaSource::Direct(data_url), staged.attachment.name.clone());
            viewer.update(cx, |viewer, cx| viewer.open(vec![item], 0, window, cx));
        }
    }

    /// Copies a staged image/SnapShot into the clipboard with its context fragment so pasting into
    /// another composer preserves `source` metadata (`pingdotgg/t3code#37771`).
    pub(super) fn copy_staged_snapshot(&self, id: &str, cx: &mut Context<Self>) {
        let Some(staged) = self.staged().iter().find(|s| s.attachment.id == id) else {
            return;
        };
        if let Some(record) = staged
            .attachment
            .context_record_with_source(staged.snapshot_source.as_ref())
        {
            let text = t3_core::composer::records::reference(&record).unwrap_or_default();
            let environment = self.environment_id(cx);
            if let Some(fragment) = t3_core::composer::records::copy_fragment(
                &text,
                &[record],
                &environment,
                self.thread_id().as_deref(),
                None,
            ) {
                super::clipboard::write(text, fragment, cx);
                return;
            }
        }
        if let Some(bytes) = &staged.bytes
            && let Some(format) = image_format(&staged.attachment.mime_type)
        {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
                format,
                (**bytes).clone(),
            )));
        }
    }

    /// `ComposerAttachmentMediaRow` and `ComposerAttachmentFileList`.
    pub(super) fn render_attachments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let staged = self.staged();
        let preparing = self.attachment_target().is_some_and(|target| self.attachments.preparations.count(&super::drafts::key(&target)) > 0);
        if staged.is_empty() && !preparing {
            return None;
        }
        let p = palette(cx);
        let rewinding = self.is_rewinding(cx);
        let file_limit = self.file_limit(cx);
        let images: Vec<&Staged> = staged.iter().filter(|staged| staged.attachment.kind == AttachmentKind::Image).collect();
        let files: Vec<&Staged> = staged.iter().filter(|staged| staged.attachment.kind == AttachmentKind::File).collect();
        const ATTACHMENT_TILE: &str = "composer-attachment-tile";
        let tiles = (!images.is_empty()).then(|| {
            h_flex().flex_wrap().gap(px(8.)).mb(px(12.)).children(images.into_iter().map(|staged| {
                let attachment = &staged.attachment;
                let id = attachment.id.clone();
                let size_text = rules::format_size(attachment.size_bytes);
                let mut tooltip = format!("{}\n{}", attachment.name, size_text);
                if let Upload::Failed { reason } = &attachment.upload { tooltip.push_str(&format!("\n\n{reason}")); }
                let status_visible = staged.needs_reattach || (attachment.upload == Upload::Uploading && staged.show_progress);
                let bottom_label = match &attachment.upload {
                    Upload::Uploading if staged.show_progress => format_upload_progress(staged.progress),
                    _ if staged.needs_reattach => "Reattach".to_owned(),
                    _ => size_text,
                };
                let can_preview = !staged.needs_reattach && (staged.bytes.is_some() || attachment.attachment_id().is_some());
                let can_copy = !staged.needs_reattach && (attachment.attachment_id().is_some() || staged.bytes.is_some());
                let preview_snapshot = (can_preview && attachment.kind == AttachmentKind::Image).then(|| Staged {
                    attachment: attachment.clone(),
                    preview: staged.preview.clone(),
                    bytes: staged.bytes.clone(),
                    progress: staged.progress,
                    show_progress: staged.show_progress,
                    upload_delay: None,
                    needs_reattach: staged.needs_reattach,
                    reattach_reason: staged.reattach_reason.clone(),
                    _copy_upload: None,
                    settled: Vec::new(),
                    source: None,
                    snapshot_source: staged.snapshot_source.clone(),
                });
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
                        .child(div().w_full().min_w_0().truncate().text_center().child(SharedString::from(attachment.name.clone())))
                        .into_any_element(),
                };
                div()
                    .id(SharedString::from(format!("attachment-{}", attachment.id)))
                    .relative()
                    .group(ATTACHMENT_TILE)
                    .size(px(64.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(hsla(p.border).opacity(0.8))
                    .bg(hsla(p.background))
                    .when(can_preview, |this| this.cursor_pointer())
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
                    .when_some(preview_snapshot, |this, preview_item| {
                        this.on_click(cx.listener(move |this, _, window, cx| {
                            this.open_staged_preview(&preview_item, window, cx);
                        }))
                    })
                    .child(thumbnail)
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .px(px(4.))
                            .bg(hsla(p.background).opacity(0.85))
                            .text_size(crate::theme::rem(crate::theme::metrics::TEXT_3XS))
                            .text_center()
                            .text_color(hsla(p.foreground))
                            .when(!status_visible, |this| this.invisible().group_hover(ATTACHMENT_TILE, |style| style.visible()))
                            .child(bottom_label),
                    )
                    .when_some((!staged.needs_reattach).then(|| failed_reason(attachment)).flatten(), |this, reason| {
                        let id = id.clone();
                        this.child(
                            div().absolute().top(px(4.)).left(px(4.)).child(
                                media_button(SharedString::from(format!("retry-{}", attachment.id)), Icon::RefreshCw, rewinding, cx)
                                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                                    .when(!rewinding, |this| {
                                        this.on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.retry_upload(id.clone(), cx);
                                        }))
                                    }),
                            ),
                        )
                    })
                    .when(can_copy && failed_reason(attachment).is_none(), |this| {
                        let copy_id = id.clone();
                        this.child(
                            div().absolute().top(px(4.)).left(px(4.)).invisible().group_hover(ATTACHMENT_TILE, |style| style.visible()).child(
                                media_button(SharedString::from(format!("copy-{}", attachment.id)), Icon::Copy, rewinding, cx)
                                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new("Copy image").build(window, cx))
                                    .when(!rewinding, |this| {
                                        this.on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.copy_staged_snapshot(&copy_id, cx);
                                        }))
                                    }),
                            ),
                        )
                    })
                    .child(
                        div().absolute().top(px(4.)).right(px(4.)).child(
                            media_button(SharedString::from(format!("remove-{}", attachment.id)), Icon::X, rewinding, cx)
                                .when(!rewinding, |this| {
                                    this.on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.remove_attachment(&id, cx);
                                    }))
                                }),
                        ),
                    )
            }))
        });
        let rows = (!files.is_empty()).then(|| {
            gpui_component::v_flex().gap(px(4.)).mb(px(12.)).children(files.into_iter().map(|staged| {
                let attachment = &staged.attachment;
                let id = attachment.id.clone();
                let can_reattach_file = file_limit.is_some_and(|limit| attachment.size_bytes <= limit);
                let status = if staged.needs_reattach {
                    if can_reattach_file { "Attach again".to_owned() } else { "Remove to send".to_owned() }
                } else {
                    match &attachment.upload {
                        Upload::Uploading if staged.show_progress => format_upload_progress(staged.progress),
                        Upload::Failed { .. } => format!("{} · upload failed", rules::format_size(attachment.size_bytes)),
                        _ => rules::format_size(attachment.size_bytes),
                    }
                };
                let downloadable_id = (!staged.needs_reattach).then(|| attachment.attachment_id().map(str::to_owned)).flatten();
                h_flex()
                    .min_w_0()
                    .gap(px(8.))
                    .py(px(4.))
                    .text_sm()
                    .text_color(hsla(p.foreground))
                    .child(Icon::File.render(px(16.), hsla(p.muted_foreground)))
                    .child(
                        div()
                            .id(SharedString::from(format!("file-open-{}", attachment.id)))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(downloadable_id.is_some(), |this| {
                                this.cursor_pointer().hover(|s| s.underline())
                            })
                            .when_some(downloadable_id, |this, aid| {
                                this.on_click(move |_, _, cx| {
                                    crate::media::open_asset(&t3_core::media::AssetResource::Attachment { attachment_id: aid.clone() }, cx);
                                })
                            })
                            .child(SharedString::from(attachment.name.clone())),
                    )
                    .child(div().flex_shrink_0().text_xs().text_color(hsla(p.secondary_label)).child(status))
                    .when_some((!staged.needs_reattach).then(|| failed_reason(attachment)).flatten(), |this, reason| {
                        let id = id.clone();
                        this.child(
                            ghost_icon_button(SharedString::from(format!("retry-{}", attachment.id)), Icon::RefreshCw, rewinding, cx)
                                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx))
                                .when(!rewinding, |this| {
                                    this.on_click(cx.listener(move |this, _, _, cx| this.retry_upload(id.clone(), cx)))
                                }),
                        )
                    })
                    .child(
                        ghost_icon_button(SharedString::from(format!("remove-{}", attachment.id)), Icon::X, rewinding, cx)
                            .when(!rewinding, |this| {
                                this.on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(&id, cx)))
                            }),
                    )
            }))
        });
        Some(gpui_component::v_flex().children(tiles).children(rows).when(preparing, |this| this.child(div().mb(px(12.)).text_xs().text_color(hsla(p.muted_foreground)).child("Preparing attachments…"))).into_any_element())
    }

    /// Drop target for files from Finder.
    pub(crate) fn on_drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach(cx) {
            if self.active_question().is_some() {
                self.error = Some(QUESTION_ATTACHMENTS_UNSUPPORTED_ERROR.to_owned());
                cx.notify();
            }
            return;
        }
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

/// The `media-close` button on a thumbnail: a small translucent surface circle (`icon-xs`, 24).
fn media_button(id: SharedString, icon: Icon, disabled: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    let fg = hsla(p.foreground);
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(hsla(p.background).opacity(0.85))
        .border_1()
        .border_color(hsla(p.border).opacity(0.6))
        .when(disabled, |this| this.opacity(0.5))
        .when(!disabled, |this| this.cursor_pointer().hover(move |this| this.bg(hsla(p.background).opacity(0.95))))
        .child(icon.render(px(14.), fg))
}

/// A ghost `icon-xs` button for a file row.
fn ghost_icon_button(id: SharedString, icon: Icon, disabled: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    div()
        .id(id)
        .size(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .when(disabled, |this| this.opacity(0.5))
        .when(!disabled, |this| this.cursor_pointer().hover(|this| this.bg(hsla(p.accent))))
        .child(icon.render(px(14.), hsla(p.muted_foreground)))
}


impl Composer {
    pub(super) fn take_draft_attachments(&mut self, target: &Target) -> Vec<Staged> {
        let mut staged = self.attachments.staged.remove(target).unwrap_or_default();
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        for entry in &mut staged {
            self.attachments.attempts.cancel(&entry.attachment.id);
            entry._copy_upload = None;
            entry.upload_delay = None;
            entry.settled.clear();
        }
        staged
    }
    pub(super) fn restore_draft_attachments(&mut self, target: &Target, from: Option<&Target>, staged: Vec<Staged>, cx: &mut Context<Self>) {
        if self.snapshot.target.as_ref() == Some(target) { self.cancel_pending_send(); }
        let uploads = self.state.read(cx).uploads.clone();
        let mut retry: Vec<String> = Vec::new();
        for entry in &staged {
            let id = entry.attachment.id.clone();
            if entry.source.is_none()
                && let Some(prev) = from
                && uploads.adopt_target(&job_key(prev, &id), job_key(target, &id), draft_target(target, &id))
            {
                continue;
            }
            if entry.attachment.attachment_id().is_none() || (from.is_some() && entry.source.is_none()) {
                retry.push(id);
            }
        }
        self.attachments.staged.insert(target.clone(), staged);
        for id in retry { self.start_upload(target.clone(), id, cx); }
    }

    /// Moves attachments staged on an unanswered (`cancelled`/`expired`) question back into the
    /// thread composer so the user does not lose their files (`pingdotgg/t3code#38005`).
    pub(super) fn return_question_attachments_to_thread(
        &mut self,
        from: &Target,
        thread_id: &str,
        staged: Vec<Staged>,
        cx: &mut Context<Self>,
    ) {
        if staged.is_empty() {
            return;
        }
        let thread_target = Target::Thread(thread_id.to_owned());
        let uploads = self.state.read(cx).uploads.clone();
        let existing_count = self.staged_for(&thread_target).len();
        let remaining_slots = rules::MAX_ATTACHMENTS.saturating_sub(existing_count);
        let mut kept = Vec::new();
        let mut overflow = Vec::new();
        for entry in staged {
            if kept.len() < remaining_slots {
                kept.push(entry);
            } else {
                overflow.push(entry);
            }
        }
        let mut retry = Vec::new();
        for entry in &kept {
            let id = entry.attachment.id.clone();
            if entry.source.is_none()
                && uploads.adopt_target(&job_key(from, &id), job_key(&thread_target, &id), draft_target(&thread_target, &id))
            {
                continue;
            }
            if entry.attachment.attachment_id().is_none() {
                retry.push(id);
            }
        }
        if !kept.is_empty() {
            self.attachments
                .staged
                .entry(thread_target.clone())
                .or_default()
                .extend(kept);
            for id in retry {
                self.start_upload(thread_target.clone(), id, cx);
            }
            self.record_draft(&thread_target, cx);
        }
        if !overflow.is_empty() {
            self.release_discarded_attachments(from, &overflow, cx);
        } else {
            // Release any leftover queue entries belonging only to `from` that were not adopted.
            uploads.release_draft(&super::drafts::key(from));
        }
    }
}

#[cfg(feature = "snapshot")]
impl Composer {
    pub(super) fn attachment_fixture(&mut self, cx: &mut Context<Self>) {
        if self.attachments.fixture_applied { return; }
        let Some(target @ Target::Thread(_)) = self.snapshot.target.clone().filter(|_| self.snapshot.loaded) else { return };
        let Some(fixture) = cx.try_global::<Fixture>().cloned() else { return };
        self.attachments.fixture_applied = true;
        let entries = fixture.0.as_array().into_iter().flatten().map(|value| {
            let field = |name| value.get(name).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
            let id = field("id");
            let upload = match field("status").as_str() {
                "failed" => Upload::Failed { reason: "Upload failed".into() },
                "uploading" => Upload::Uploading,
                _ => Upload::Ready { attachment_id: format!("fixture-{id}") },
            };
            let held = (upload == Upload::Uploading && value["hold"].as_bool() == Some(true)).then(|| cx.spawn(async |_, _| futures::future::pending::<()>().await));
            Staged {
                attachment: Attachment { id, name: field("name"), kind: if field("type") == "image" { AttachmentKind::Image } else { AttachmentKind::File }, mime_type: field("mimeType"), size_bytes: value["sizeBytes"].as_u64().unwrap_or_default(), pasted_text: false, upload },
                preview: None, bytes: None,
                progress: value["progress"].as_f64().unwrap_or(0.0),
                show_progress: value["progress"].as_f64().is_some(),
                upload_delay: None,
                needs_reattach: value["needsReattach"].as_bool().unwrap_or(false),
                reattach_reason: None,
                _copy_upload: held, settled: Vec::new(), source: None, snapshot_source: None,
            }
        }).collect();
        self.attachments.staged.insert(target, entries);
    }
}
