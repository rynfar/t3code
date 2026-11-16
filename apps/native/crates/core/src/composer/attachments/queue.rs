//! Pure-Rust Attachment Upload Queue & State Machine (`CO-ATTACH` transport layer).
//!
//! Ports `apps/web/src/lib/attachmentUploadState.ts`,
//! `apps/web/src/lib/attachmentUploadQueue.ts`, and
//! `packages/client-runtime/src/state/attachments.ts`.

use std::collections::{HashMap, VecDeque};

use super::AttachmentKind;

/// Maximum concurrent attachment uploads allowed per environment
/// (`MAX_UPLOADS_PER_ENVIRONMENT` in `attachmentUploadQueue.ts`).
pub const MAX_UPLOADS_PER_ENVIRONMENT: usize = 3;

/// Per-upload HTTP transfer timeout in milliseconds
/// (`UPLOAD_TIMEOUT_MS` in `attachmentUploadQueue.ts`).
pub const UPLOAD_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// Quantization buckets matching `Math.floor(progress * 20)` in `attachmentUploadQueue.ts`.
pub const PROGRESS_STEP_BUCKETS: i32 = 20;

/// Monotonic token distinguishing individual upload job attempts for the same `job_key`.
pub type JobToken = u64;

/// A server-side ready attachment tied to an environment (`ReadyAttachmentUpload`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyAttachmentUpload {
    pub environment_id: String,
    pub attachment_id: String,
}

/// Rich attachment upload state (`AttachmentUploadState` in `attachmentUploadState.ts`).
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentUploadState {
    Uploading {
        environment_id: String,
        progress: f64,
        previous: Option<ReadyAttachmentUpload>,
    },
    Ready(ReadyAttachmentUpload),
    Failed {
        environment_id: String,
        reason: String,
        /// Non-persisted minted `attachmentId` left on the server after a failed HTTP transfer,
        /// cleaned up on retry or release.
        attachment_id: Option<String>,
        previous: Option<ReadyAttachmentUpload>,
    },
}

impl AttachmentUploadState {
    pub fn environment_id(&self) -> &str {
        match self {
            Self::Uploading { environment_id, .. } | Self::Failed { environment_id, .. } => {
                environment_id
            }
            Self::Ready(ready) => &ready.environment_id,
        }
    }

    pub fn ready_attachment(&self) -> Option<&ReadyAttachmentUpload> {
        match self {
            Self::Ready(ready) => Some(ready),
            Self::Uploading { .. } | Self::Failed { .. } => None,
        }
    }

    pub fn ready_attachment_id(&self) -> Option<&str> {
        self.ready_attachment().map(|ready| ready.attachment_id.as_str())
    }

    pub fn previous_ready(&self) -> Option<&ReadyAttachmentUpload> {
        match self {
            Self::Uploading { previous, .. } | Self::Failed { previous, .. } => previous.as_ref(),
            Self::Ready(_) => None,
        }
    }

    pub fn to_simple_upload(&self) -> super::Upload {
        match self {
            Self::Uploading { .. } => super::Upload::Uploading,
            Self::Ready(ready) => super::Upload::Ready {
                attachment_id: ready.attachment_id.clone(),
            },
            Self::Failed { reason, .. } => super::Upload::Failed {
                reason: reason.clone(),
            },
        }
    }
}

/// Returns whether staged attachments block sending (`attachmentUploadBlockReason` in
/// `attachmentUploadState.ts`). Failed uploads do NOT block send because send retries them.
pub fn rich_upload_block_reason<'a>(
    attachments: impl IntoIterator<Item = (AttachmentKind, Option<&'a AttachmentUploadState>)>,
) -> Option<&'static str> {
    for (kind, upload) in attachments {
        if kind != AttachmentKind::File {
            continue;
        }
        match upload {
            None | Some(AttachmentUploadState::Uploading { .. }) => {
                return Some("Wait for file attachments to finish uploading.");
            }
            Some(AttachmentUploadState::Ready(_)) | Some(AttachmentUploadState::Failed { .. }) => {}
        }
    }
    None
}

/// Formats `[0.0, 1.0]` progress as a floor-rounded integer percentage (`"0%"..="100%"`),
/// clamping non-finite and out-of-range inputs (`formatAttachmentUploadProgress`).
pub fn format_upload_progress(progress: f64) -> String {
    let bounded = clamp_progress(progress);
    let pct = (bounded * 100.0).floor() as u8;
    format!("{pct}%")
}

/// Maps `[0.0, 1.0]` progress into `0..=20` discrete 5% steps (`Math.floor(progress * 20)`).
pub fn progress_step(progress: f64) -> i32 {
    let bounded = clamp_progress(progress);
    (bounded * f64::from(PROGRESS_STEP_BUCKETS)).floor() as i32
}

/// Quantizes `[0.0, 1.0]` progress onto 5% boundaries matching web's progress throttling.
pub fn quantize_upload_progress(progress: f64) -> f64 {
    f64::from(progress_step(progress)) / f64::from(PROGRESS_STEP_BUCKETS)
}

fn clamp_progress(progress: f64) -> f64 {
    if !progress.is_finite() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    }
}

/// Checks whether a server RPC failure tag denotes a missing/expired attachment
/// (`isAssetAttachmentNotFoundFailure` in `packages/client-runtime/src/state/attachments.ts`).
pub fn is_asset_attachment_not_found_tag(tag: Option<&str>) -> bool {
    tag == Some("AssetAttachmentNotFoundError")
}

/// Outcome of `assets.createUrl` verification for a persisted attachment ID on reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedAttachmentVerification {
    Verified,
    Missing { reason: String },
    Failed { reason: String },
}

/// Stage within `runAttachmentUploadCycle` where an upload failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadCycleStep {
    CreateUploadUrl,
    ResolveUploadUrl,
    UploadBytes,
}

/// Outcome returned by `runAttachmentUploadCycle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadCycleOutcome {
    Uploaded {
        attachment_id: String,
    },
    Cancelled {
        minted_attachment_id: Option<String>,
    },
    Failed {
        step: UploadCycleStep,
        /// Preserved minted ID when transfer failed after `createUploadUrl`, so caller can
        /// clean it up on retry or release rather than leaking pending uploads.
        attachment_id: Option<String>,
        error: String,
    },
}

/// Identifies the composer or question draft currently owning a file upload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DraftTarget {
    pub draft_id: String,
    pub file_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobPhase {
    Queued,
    Active,
    Settled,
}

#[derive(Debug, Clone)]
struct QueueJob {
    token: JobToken,
    job_key: String,
    environment_id: String,
    target: DraftTarget,
    has_bytes: bool,
    persisted_attachment_id: Option<String>,
    restore_on_cancel: Option<ReadyAttachmentUpload>,
    state: AttachmentUploadState,
    phase: JobPhase,
    last_progress_step: i32,
    waiting_for_reconnect: bool,
}

/// Actions emitted by `UploadQueue` transitions for the async client runner to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueEffect {
    /// Dispatch verification (if `persisted_attachment_id.is_some()`) or full upload cycle.
    StartUpload {
        job_token: JobToken,
        job_key: String,
        environment_id: String,
        persisted_attachment_id: Option<String>,
        has_bytes: bool,
    },
    /// Abort the currently active HTTP/RPC upload future for `job_token`.
    AbortActive {
        job_token: JobToken,
        job_key: String,
        environment_id: String,
    },
    /// Call `attachments.delete` for an orphaned, replaced, or discarded pending attachment.
    DeletePendingAttachment {
        environment_id: String,
        attachment_id: String,
    },
    /// Upload progress quantized to a new 5% step.
    NotifyProgress {
        job_key: String,
        target: DraftTarget,
        state: AttachmentUploadState,
    },
    /// Persisted attachment disappeared on server and local bytes are absent (needs reattach).
    MarkMissingPersistedFile {
        job_key: String,
        target: DraftTarget,
        reason: String,
    },
    /// Authoritative upload state update (`None` clears metadata on release/cancel without restore).
    StateChanged {
        job_key: String,
        target: DraftTarget,
        state: Option<AttachmentUploadState>,
    },
    /// Job reached a terminal state for its current attempt (`Ready`, `Failed`, or removed).
    Settled {
        job_key: String,
        environment_id: String,
    },
}

/// Deterministic scheduler & state machine managing attachment upload concurrency
/// (`MAX_UPLOADS_PER_ENVIRONMENT = 3`), draft adoption/retargeting, cancellation cleanup,
/// retry-on-send, and reconnect auto-retry.
#[derive(Debug, Default)]
pub struct UploadQueue {
    next_token: JobToken,
    jobs: HashMap<String, QueueJob>,
    pending_order: VecDeque<String>,
}

impl UploadQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self, job_key: &str) -> Option<&AttachmentUploadState> {
        self.jobs.get(job_key).map(|job| &job.state)
    }

    pub fn target(&self, job_key: &str) -> Option<&DraftTarget> {
        self.jobs.get(job_key).map(|job| &job.target)
    }

    pub fn token(&self, job_key: &str) -> Option<JobToken> {
        self.jobs.get(job_key).map(|job| job.token)
    }

    pub fn active_count(&self, environment_id: &str) -> usize {
        self.jobs
            .values()
            .filter(|job| job.environment_id == environment_id && job.phase == JobPhase::Active)
            .count()
    }

    pub fn queued_count(&self, environment_id: &str) -> usize {
        self.jobs
            .values()
            .filter(|job| job.environment_id == environment_id && job.phase == JobPhase::Queued)
            .count()
    }

    pub fn is_pending(&self, job_key: &str) -> bool {
        self.jobs
            .get(job_key)
            .is_some_and(|job| matches!(job.phase, JobPhase::Queued | JobPhase::Active))
    }

    pub fn has_pending_for_keys<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> bool {
        keys.into_iter().any(|key| self.is_pending(key))
    }

    pub fn job_keys_for_draft(&self, draft_id: &str) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|(_, job)| job.target.draft_id == draft_id)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Starts or adopts an upload for `job_key`.
    ///
    /// Matches `startAttachmentUpload` in `attachmentUploadQueue.ts`:
    /// - If a live upload (`Queued` or `Active`) already exists on the same `environment_id`,
    ///   rebinds `target` (and upgrades `has_bytes` if needed) without duplicating work.
    /// - If an active upload is running on a different environment, aborts it and preserves any
    ///   prior ready upload across retargets so cancellation can restore it.
    pub fn start(
        &mut self,
        job_key: String,
        target: DraftTarget,
        environment_id: String,
        has_bytes: bool,
        persisted_attachment_id: Option<String>,
        current_state: Option<AttachmentUploadState>,
    ) -> (Option<JobToken>, Vec<QueueEffect>) {
        let mut effects = Vec::new();

        if let Some(existing) = self.jobs.get_mut(&job_key)
            && matches!(existing.phase, JobPhase::Queued | JobPhase::Active)
        {
            if existing.environment_id == environment_id
                && (!has_bytes || existing.has_bytes || persisted_attachment_id.is_none())
            {
                existing.target = target;
                return (Some(existing.token), effects);
            }
            // Retargeted across environments or upgrading source bytes: cancel existing first.
            let prev_restore = existing.restore_on_cancel.take();
            let cancel_effects = self.cancel_internal(&job_key, false, false);
            effects.extend(cancel_effects);
            if let Some(prev) = prev_restore {
                let state = AttachmentUploadState::Uploading {
                    environment_id: environment_id.clone(),
                    progress: 0.0,
                    previous: Some(prev.clone()),
                };
                return self.start_with_previous(
                    job_key,
                    target,
                    environment_id,
                    has_bytes,
                    persisted_attachment_id,
                    Some(prev),
                    state,
                    effects,
                );
            }
        }

        if !has_bytes && persisted_attachment_id.is_none() {
            effects.push(QueueEffect::MarkMissingPersistedFile {
                job_key: job_key.clone(),
                target: target.clone(),
                reason: "File bytes are missing and no persisted attachment ID is available.".to_owned(),
            });
            effects.push(QueueEffect::StateChanged {
                job_key,
                target,
                state: None,
            });
            return (None, effects);
        }

        // Delete any stale failed mint from prior settled job for the same key before starting fresh.
        if let Some(existing) = self.jobs.get(&job_key)
            && let AttachmentUploadState::Failed {
                environment_id: prev_env,
                attachment_id: Some(failed_id),
                ..
            } = &existing.state
        {
            effects.push(QueueEffect::DeletePendingAttachment {
                environment_id: prev_env.clone(),
                attachment_id: failed_id.clone(),
            });
        }

        let previous = current_state
            .as_ref()
            .and_then(|s| s.ready_attachment().or_else(|| s.previous_ready()))
            .cloned();

        let state = AttachmentUploadState::Uploading {
            environment_id: environment_id.clone(),
            progress: 0.0,
            previous: previous.clone(),
        };

        self.start_with_previous(
            job_key,
            target,
            environment_id,
            has_bytes,
            persisted_attachment_id,
            previous,
            state,
            effects,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_with_previous(
        &mut self,
        job_key: String,
        target: DraftTarget,
        environment_id: String,
        has_bytes: bool,
        persisted_attachment_id: Option<String>,
        restore_on_cancel: Option<ReadyAttachmentUpload>,
        state: AttachmentUploadState,
        mut effects: Vec<QueueEffect>,
    ) -> (Option<JobToken>, Vec<QueueEffect>) {
        self.next_token += 1;
        let token = self.next_token;

        let job = QueueJob {
            token,
            job_key: job_key.clone(),
            environment_id,
            target: target.clone(),
            has_bytes,
            persisted_attachment_id,
            restore_on_cancel,
            state: state.clone(),
            phase: JobPhase::Queued,
            last_progress_step: 0,
            waiting_for_reconnect: false,
        };

        self.jobs.insert(job_key.clone(), job);
        self.pending_order.retain(|k| k != &job_key);
        self.pending_order.push_back(job_key.clone());

        effects.push(QueueEffect::StateChanged {
            job_key,
            target,
            state: Some(state),
        });
        effects.extend(self.pump());
        (Some(token), effects)
    }

    /// Promotes queued jobs into active slots up to `MAX_UPLOADS_PER_ENVIRONMENT` per environment,
    /// preserving FIFO ordering within each environment without allowing one saturated environment
    /// to head-of-line block another environment.
    pub fn pump(&mut self) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        let mut active_counts: HashMap<String, usize> = HashMap::new();
        for job in self.jobs.values() {
            if job.phase == JobPhase::Active {
                *active_counts.entry(job.environment_id.clone()).or_insert(0) += 1;
            }
        }

        let mut remaining = VecDeque::with_capacity(self.pending_order.len());
        while let Some(key) = self.pending_order.pop_front() {
            let Some(job) = self.jobs.get_mut(&key) else {
                continue;
            };
            if job.phase != JobPhase::Queued {
                continue;
            }
            let active = active_counts.get(&job.environment_id).copied().unwrap_or(0);
            if active < MAX_UPLOADS_PER_ENVIRONMENT {
                *active_counts.entry(job.environment_id.clone()).or_insert(0) += 1;
                job.phase = JobPhase::Active;
                effects.push(QueueEffect::StartUpload {
                    job_token: job.token,
                    job_key: job.job_key.clone(),
                    environment_id: job.environment_id.clone(),
                    persisted_attachment_id: job.persisted_attachment_id.clone(),
                    has_bytes: job.has_bytes,
                });
            } else {
                remaining.push_back(key);
            }
        }
        self.pending_order = remaining;
        effects
    }

    /// Records raw upload progress for `job_token`, emitting `NotifyProgress` only when the
    /// 5%-quantized step changes (`lastProgressStep` throttling).
    pub fn update_progress(&mut self, job_token: JobToken, raw_progress: f64) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        let Some(job) = self
            .jobs
            .values_mut()
            .find(|job| job.token == job_token && job.phase == JobPhase::Active)
        else {
            return effects;
        };
        let step = progress_step(raw_progress);
        if step <= job.last_progress_step {
            return effects;
        }
        job.last_progress_step = step;
        let quantized = quantize_upload_progress(raw_progress);
        if let AttachmentUploadState::Uploading { progress, .. } = &mut job.state {
            *progress = quantized;
        }
        effects.push(QueueEffect::NotifyProgress {
            job_key: job.job_key.clone(),
            target: job.target.clone(),
            state: job.state.clone(),
        });
        effects
    }

    /// Handles `verifyPersistedAttachmentUpload` result for an active verification job.
    ///
    /// Returns `(continue_with_byte_upload, effects)`:
    /// - `true` when verification returned `Missing` AND the job has local bytes to upload
    ///   using the same active slot.
    /// - `false` when the job transitioned (`Verified`, `Failed`, or `Missing` without bytes).
    pub fn complete_persisted_verification(
        &mut self,
        job_token: JobToken,
        result: PersistedAttachmentVerification,
    ) -> (bool, Vec<QueueEffect>) {
        let mut effects = Vec::new();
        let Some(key) = self
            .jobs
            .iter()
            .find(|(_, job)| job.token == job_token && job.phase == JobPhase::Active)
            .map(|(k, _)| k.clone())
        else {
            return (false, effects);
        };

        let Some(job) = self.jobs.get_mut(&key) else {
            return (false, effects);
        };

        match result {
            PersistedAttachmentVerification::Verified => {
                let Some(attachment_id) = job.persisted_attachment_id.take() else {
                    return (false, effects);
                };
                if let Some(prev) = job.restore_on_cancel.take()
                    && (prev.attachment_id != attachment_id || prev.environment_id != job.environment_id)
                {
                    effects.push(QueueEffect::DeletePendingAttachment {
                        environment_id: prev.environment_id,
                        attachment_id: prev.attachment_id,
                    });
                }
                let ready = AttachmentUploadState::Ready(ReadyAttachmentUpload {
                    environment_id: job.environment_id.clone(),
                    attachment_id,
                });
                job.state = ready.clone();
                job.phase = JobPhase::Settled;
                job.waiting_for_reconnect = false;
                let target = job.target.clone();
                let env_id = job.environment_id.clone();
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: Some(ready),
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
                effects.extend(self.pump());
                (false, effects)
            }
            PersistedAttachmentVerification::Missing { reason } => {
                job.persisted_attachment_id = None;
                if job.has_bytes {
                    // Fall through to full upload cycle in the same active slot.
                    return (true, effects);
                }
                let target = job.target.clone();
                let env_id = job.environment_id.clone();
                let prev = job.restore_on_cancel.take();
                if let Some(prev_ready) = prev {
                    effects.push(QueueEffect::DeletePendingAttachment {
                        environment_id: prev_ready.environment_id,
                        attachment_id: prev_ready.attachment_id,
                    });
                }
                self.jobs.remove(&key);
                effects.push(QueueEffect::MarkMissingPersistedFile {
                    job_key: key.clone(),
                    target: target.clone(),
                    reason,
                });
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: None,
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
                effects.extend(self.pump());
                (false, effects)
            }
            PersistedAttachmentVerification::Failed { reason } => {
                // Critical rule: transient failure on a persisted attachment MUST NOT record
                // `persisted_attachment_id` as a deletable failed mint ID!
                let env_id = job.environment_id.clone();
                let target = job.target.clone();
                let failed = AttachmentUploadState::Failed {
                    environment_id: env_id.clone(),
                    reason,
                    attachment_id: None,
                    previous: job.restore_on_cancel.clone(),
                };
                job.state = failed.clone();
                job.phase = JobPhase::Settled;
                job.waiting_for_reconnect = true;
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: Some(failed),
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
                effects.extend(self.pump());
                (false, effects)
            }
        }
    }

    /// Completes `runAttachmentUploadCycle` for `job_token`.
    ///
    /// If `job_token` is no longer active (superseded/cancelled while in flight), any newly minted
    /// server attachment from this stale attempt is queued for `DeletePendingAttachment`.
    pub fn complete_cycle(&mut self, job_token: JobToken, outcome: UploadCycleOutcome) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        let active_key = self
            .jobs
            .iter()
            .find(|(_, job)| job.token == job_token && job.phase == JobPhase::Active)
            .map(|(k, _)| k.clone());

        let Some(key) = active_key else {
            // Stale completion after cancellation/supersession: clean up any newly minted ID.
            let orphaned_env = self
                .jobs
                .values()
                .find(|job| job.token == job_token)
                .map(|job| job.environment_id.clone());
            if let Some(env_id) = orphaned_env {
                match outcome {
                    UploadCycleOutcome::Uploaded { attachment_id }
                    | UploadCycleOutcome::Cancelled {
                        minted_attachment_id: Some(attachment_id),
                    }
                    | UploadCycleOutcome::Failed {
                        attachment_id: Some(attachment_id),
                        ..
                    } => {
                        effects.push(QueueEffect::DeletePendingAttachment {
                            environment_id: env_id,
                            attachment_id,
                        });
                    }
                    _ => {}
                }
            }
            return effects;
        };

        let Some(job) = self.jobs.get_mut(&key) else {
            return effects;
        };

        let env_id = job.environment_id.clone();
        let target = job.target.clone();

        match outcome {
            UploadCycleOutcome::Uploaded { attachment_id } => {
                if let Some(prev) = job.restore_on_cancel.take()
                    && (prev.attachment_id != attachment_id || prev.environment_id != env_id)
                {
                    effects.push(QueueEffect::DeletePendingAttachment {
                        environment_id: prev.environment_id,
                        attachment_id: prev.attachment_id,
                    });
                }
                let ready = AttachmentUploadState::Ready(ReadyAttachmentUpload {
                    environment_id: env_id.clone(),
                    attachment_id,
                });
                job.state = ready.clone();
                job.phase = JobPhase::Settled;
                job.waiting_for_reconnect = false;
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: Some(ready),
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
            }
            UploadCycleOutcome::Cancelled {
                minted_attachment_id,
            } => {
                if let Some(minted) = minted_attachment_id {
                    effects.push(QueueEffect::DeletePendingAttachment {
                        environment_id: env_id.clone(),
                        attachment_id: minted,
                    });
                }
                let restored = job.restore_on_cancel.take().map(AttachmentUploadState::Ready);
                self.jobs.remove(&key);
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: restored,
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
            }
            UploadCycleOutcome::Failed {
                attachment_id,
                error,
                ..
            } => {
                let failed = AttachmentUploadState::Failed {
                    environment_id: env_id.clone(),
                    reason: error,
                    attachment_id,
                    previous: job.restore_on_cancel.clone(),
                };
                job.state = failed.clone();
                job.phase = JobPhase::Settled;
                job.waiting_for_reconnect = true;
                effects.push(QueueEffect::StateChanged {
                    job_key: key.clone(),
                    target,
                    state: Some(failed),
                });
                effects.push(QueueEffect::Settled {
                    job_key: key,
                    environment_id: env_id,
                });
            }
        }

        effects.extend(self.pump());
        effects
    }

    /// Cancels an upload job. When `restore_previous` is `true`, any prior `Ready` upload is
    /// restored into `StateChanged` rather than deleted (used when a user stops retargeting);
    /// otherwise any failed mint or ready attachment is cleaned up (`release`).
    pub fn cancel(&mut self, job_key: &str, restore_previous: bool) -> Vec<QueueEffect> {
        self.cancel_internal(job_key, restore_previous, true)
    }

    fn cancel_internal(
        &mut self,
        job_key: &str,
        restore_previous: bool,
        run_pump: bool,
    ) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        self.pending_order.retain(|k| k != job_key);
        let Some(job) = self.jobs.remove(job_key) else {
            return effects;
        };

        if job.phase == JobPhase::Active {
            effects.push(QueueEffect::AbortActive {
                job_token: job.token,
                job_key: job.job_key.clone(),
                environment_id: job.environment_id.clone(),
            });
        }

        if let AttachmentUploadState::Failed {
            environment_id,
            attachment_id: Some(failed_id),
            ..
        } = &job.state
        {
            effects.push(QueueEffect::DeletePendingAttachment {
                environment_id: environment_id.clone(),
                attachment_id: failed_id.clone(),
            });
        }

        let next_state = if restore_previous {
            job.restore_on_cancel.map(AttachmentUploadState::Ready)
        } else {
            if let AttachmentUploadState::Ready(ready) = &job.state {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: ready.environment_id.clone(),
                    attachment_id: ready.attachment_id.clone(),
                });
            }
            if let Some(prev) = job.restore_on_cancel {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: prev.environment_id,
                    attachment_id: prev.attachment_id,
                });
            }
            None
        };

        effects.push(QueueEffect::StateChanged {
            job_key: job.job_key.clone(),
            target: job.target,
            state: next_state,
        });
        if matches!(job.phase, JobPhase::Queued | JobPhase::Active) {
            effects.push(QueueEffect::Settled {
                job_key: job.job_key,
                environment_id: job.environment_id,
            });
        }

        if run_pump {
            effects.extend(self.pump());
        }
        effects
    }

    /// Releases an attachment upload when removed from a draft (`releaseAttachmentUpload`).
    pub fn release(&mut self, job_key: &str) -> Vec<QueueEffect> {
        self.cancel(job_key, false)
    }

    /// Releases all attachments associated with `draft_id` (`releaseDraftAttachments`).
    pub fn release_draft(&mut self, draft_id: &str) -> Vec<QueueEffect> {
        let keys = self.job_keys_for_draft(draft_id);
        let mut effects = Vec::new();
        for key in keys {
            effects.extend(self.release(&key));
        }
        effects
    }

    /// Moves/adopts a live or settled attachment job to a new `DraftTarget` (for example, when a
    /// new-thread draft is promoted to a persisted thread or returned to a draft) without aborting
    /// the upload or deleting its server asset.
    pub fn adopt_target(&mut self, old_job_key: &str, new_job_key: String, new_target: DraftTarget) -> bool {
        let Some(mut job) = self.jobs.remove(old_job_key) else {
            return false;
        };
        job.job_key = new_job_key.clone();
        job.target = new_target;
        self.jobs.insert(new_job_key.clone(), job);
        for key in &mut self.pending_order {
            if key == old_job_key {
                *key = new_job_key.clone();
            }
        }
        true
    }

    /// Retries a single failed upload (`retryAttachmentUpload`), cleaning up any previous failed
    /// minted `attachment_id` first.
    pub fn retry(&mut self, job_key: &str) -> (Option<JobToken>, Vec<QueueEffect>) {
        let Some(existing) = self.jobs.get(job_key).cloned() else {
            return (None, Vec::new());
        };
        if matches!(existing.phase, JobPhase::Queued | JobPhase::Active) {
            return (Some(existing.token), Vec::new());
        }
        self.start(
            job_key.to_owned(),
            existing.target,
            existing.environment_id,
            existing.has_bytes,
            existing.persisted_attachment_id,
            Some(existing.state),
        )
    }

    /// Retries every failed upload in `job_keys` on send (`retry-on-send` in `useComposerSend.ts`).
    pub fn retry_on_send(&mut self, job_keys: &[String]) -> (Vec<JobToken>, Vec<QueueEffect>) {
        let mut tokens = Vec::new();
        let mut effects = Vec::new();
        for key in job_keys {
            let Some(job) = self.jobs.get(key) else {
                continue;
            };
            match &job.state {
                AttachmentUploadState::Failed { .. } => {
                    let (token, mut step_effects) = self.retry(key);
                    if let Some(t) = token {
                        tokens.push(t);
                    }
                    effects.append(&mut step_effects);
                }
                AttachmentUploadState::Uploading { .. } => {
                    tokens.push(job.token);
                }
                AttachmentUploadState::Ready(_) => {}
            }
        }
        (tokens, effects)
    }

    /// Reconnect hook matching `attachmentUploadQueue.ts:464-478`: retries failed uploads on
    /// `environment_id` strictly on a `!was_connected && is_connected` edge when the job is still
    /// settled in `Failed` with `waiting_for_reconnect`.
    pub fn on_connection_transition(
        &mut self,
        environment_id: &str,
        was_connected: bool,
        is_connected: bool,
    ) -> Vec<QueueEffect> {
        if was_connected || !is_connected {
            return Vec::new();
        }
        let keys_to_retry: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, job)| {
                job.environment_id == environment_id
                    && job.phase == JobPhase::Settled
                    && job.waiting_for_reconnect
                    && matches!(job.state, AttachmentUploadState::Failed { .. })
            })
            .map(|(k, _)| k.clone())
            .collect();

        let mut effects = Vec::new();
        for key in keys_to_retry {
            let (_, mut retry_effects) = self.retry(&key);
            effects.append(&mut retry_effects);
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(draft: &str, file: &str) -> DraftTarget {
        DraftTarget {
            draft_id: draft.to_owned(),
            file_id: file.to_owned(),
        }
    }

    #[test]
    fn formats_and_quantizes_progress_and_checks_block_reason() {
        assert_eq!(format_upload_progress(f64::NAN), "0%");
        assert_eq!(format_upload_progress(-0.2), "0%");
        assert_eq!(format_upload_progress(0.499), "49%");
        assert_eq!(format_upload_progress(1.5), "100%");
        assert_eq!(progress_step(0.049), 0);
        assert_eq!(progress_step(0.05), 1);
        assert_eq!(quantize_upload_progress(0.54), 0.5);

        let uploading = AttachmentUploadState::Uploading {
            environment_id: "env-a".into(),
            progress: 0.2,
            previous: None,
        };
        let failed = AttachmentUploadState::Failed {
            environment_id: "env-a".into(),
            reason: "offline".into(),
            attachment_id: None,
            previous: None,
        };
        let ready = AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: "env-a".into(),
            attachment_id: "att-1".into(),
        });

        assert_eq!(
            rich_upload_block_reason([(AttachmentKind::File, Some(&uploading))]),
            Some("Wait for file attachments to finish uploading.")
        );
        // Failed file uploads do not block send because send retries them.
        assert_eq!(
            rich_upload_block_reason([
                (AttachmentKind::File, Some(&failed)),
                (AttachmentKind::File, Some(&ready)),
            ]),
            None
        );
        // Image attachments without file upload state do not trigger the file block message.
        assert_eq!(rich_upload_block_reason([(AttachmentKind::Image, None)]), None);
    }

    #[test]
    fn enforces_max_3_per_environment_without_blocking_other_environments() {
        let mut queue = UploadQueue::new();
        let mut tokens_a = Vec::new();
        for idx in 0..4 {
            let (tok, _) = queue.start(
                format!("env-a-{idx}"),
                target("d1", &format!("f-{idx}")),
                "env-a".into(),
                true,
                None,
                None,
            );
            tokens_a.push(tok.unwrap());
        }
        assert_eq!(queue.active_count("env-a"), 3);
        assert_eq!(queue.queued_count("env-a"), 1);

        // Another environment still starts immediately despite env-a being saturated.
        let (tok_b, effects_b) = queue.start(
            "env-b-0".into(),
            target("d2", "f-b0"),
            "env-b".into(),
            true,
            None,
            None,
        );
        assert!(tok_b.is_some());
        assert_eq!(queue.active_count("env-b"), 1);
        assert!(effects_b.iter().any(|e| matches!(
            e,
            QueueEffect::StartUpload { job_key, .. } if job_key == "env-b-0"
        )));

        // Progress updates throttle at 5% boundaries.
        assert!(queue.update_progress(tokens_a[0], 0.02).is_empty());
        assert_eq!(queue.update_progress(tokens_a[0], 0.05).len(), 1);
        assert!(queue.update_progress(tokens_a[0], 0.08).is_empty());

        // Completing one active env-a job promotes the 4th queued job.
        let complete_effects = queue.complete_cycle(
            tokens_a[0],
            UploadCycleOutcome::Uploaded {
                attachment_id: "att-0".into(),
            },
        );
        assert_eq!(queue.active_count("env-a"), 3);
        assert_eq!(queue.queued_count("env-a"), 0);
        assert!(complete_effects.iter().any(|e| matches!(
            e,
            QueueEffect::StartUpload { job_key, .. } if job_key == "env-a-3"
        )));
    }

    #[test]
    fn reconnect_retries_failed_jobs_and_retry_deletes_stale_failed_mint() {
        let mut queue = UploadQueue::new();
        let (tok, _) = queue.start(
            "job-1".into(),
            target("d1", "f1"),
            "env-1".into(),
            true,
            None,
            None,
        );
        let tok = tok.unwrap();

        let fail_effects = queue.complete_cycle(
            tok,
            UploadCycleOutcome::Failed {
                step: UploadCycleStep::UploadBytes,
                attachment_id: Some("mint-orphan-1".into()),
                error: "network dropped".into(),
            },
        );
        assert!(!fail_effects.iter().any(|e| matches!(e, QueueEffect::DeletePendingAttachment { .. })));

        // Non-transition does not retry.
        assert!(queue.on_connection_transition("env-1", true, true).is_empty());
        // Transition from disconnected -> connected retries and deletes stale mint-orphan-1.
        let reconnect_effects = queue.on_connection_transition("env-1", false, true);
        assert!(reconnect_effects.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "mint-orphan-1"
        )));
        assert!(reconnect_effects.iter().any(|e| matches!(
            e,
            QueueEffect::StartUpload { job_key, .. } if job_key == "job-1"
        )));
    }

    #[test]
    fn persisted_verification_handles_missing_and_transient_failure_safely() {
        let mut queue = UploadQueue::new();

        // Persisted attachment with no local bytes that fails transiently stays Failed with attachment_id=None.
        let (tok1, _) = queue.start(
            "persisted-transient".into(),
            target("d1", "f1"),
            "env-1".into(),
            false,
            Some("persisted-id-1".into()),
            None,
        );
        let (cont, eff1) = queue.complete_persisted_verification(
            tok1.unwrap(),
            PersistedAttachmentVerification::Failed {
                reason: "rpc closed".into(),
            },
        );
        assert!(!cont);
        assert!(matches!(
            queue.state("persisted-transient"),
            Some(AttachmentUploadState::Failed {
                attachment_id: None,
                ..
            })
        ));
        assert!(!eff1.iter().any(|e| matches!(e, QueueEffect::DeletePendingAttachment { .. })));

        // Missing persisted attachment with NO local bytes marks needs-reattach.
        let (tok2, _) = queue.start(
            "persisted-missing".into(),
            target("d1", "f2"),
            "env-1".into(),
            false,
            Some("persisted-id-2".into()),
            None,
        );
        let (cont2, eff2) = queue.complete_persisted_verification(
            tok2.unwrap(),
            PersistedAttachmentVerification::Missing {
                reason: "expired".into(),
            },
        );
        assert!(!cont2);
        assert!(eff2.iter().any(|e| matches!(
            e,
            QueueEffect::MarkMissingPersistedFile { job_key, .. } if job_key == "persisted-missing"
        )));

        // Missing persisted attachment WITH local bytes continues into full upload cycle.
        let (tok3, _) = queue.start(
            "persisted-reupload".into(),
            target("d1", "f3"),
            "env-1".into(),
            true,
            Some("persisted-id-3".into()),
            None,
        );
        let (cont3, _) = queue.complete_persisted_verification(
            tok3.unwrap(),
            PersistedAttachmentVerification::Missing {
                reason: "expired".into(),
            },
        );
        assert!(cont3);
        assert_eq!(queue.active_count("env-1"), 1);
    }

    #[test]
    fn cancel_restores_previous_ready_and_adoption_preserves_live_upload() {
        let mut queue = UploadQueue::new();
        let prev_ready = AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: "env-old".into(),
            attachment_id: "att-old".into(),
        });

        let (tok, _) = queue.start(
            "retarget-job".into(),
            target("draft-1", "f1"),
            "env-new".into(),
            true,
            None,
            Some(prev_ready.clone()),
        );
        let _ = tok.unwrap();

        // Adopt to promoted draft target while active.
        assert!(queue.adopt_target(
            "retarget-job",
            "retarget-job-adopted".into(),
            target("thread-draft-1", "f1")
        ));
        assert_eq!(queue.target("retarget-job-adopted"), Some(&target("thread-draft-1", "f1")));

        // Cancel with restore_previous=true restores att-old rather than deleting it.
        let cancel_effects = queue.cancel("retarget-job-adopted", true);
        assert!(cancel_effects.iter().any(|e| matches!(e, QueueEffect::AbortActive { .. })));
        assert!(!cancel_effects.iter().any(|e| matches!(e, QueueEffect::DeletePendingAttachment { .. })));
        assert!(cancel_effects.iter().any(|e| matches!(
            e,
            QueueEffect::StateChanged { state: Some(s), .. } if s == &prev_ready
        )));

        // Retry on send only retries failed items.
        let (tok_fail, _) = queue.start(
            "failed-job".into(),
            target("d", "f-fail"),
            "env-1".into(),
            true,
            None,
            None,
        );
        let _ = queue.complete_cycle(
            tok_fail.unwrap(),
            UploadCycleOutcome::Failed {
                step: UploadCycleStep::UploadBytes,
                attachment_id: Some("mint-fail".into()),
                error: "timeout".into(),
            },
        );
        let (retry_tokens, retry_eff) = queue.retry_on_send(&["failed-job".into()]);
        assert_eq!(retry_tokens.len(), 1);
        assert!(retry_eff.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "mint-fail"
        )));
    }
}
