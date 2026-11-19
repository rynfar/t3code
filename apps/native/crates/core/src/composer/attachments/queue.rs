//! Pure-Rust Attachment Upload Queue & State Machine (`CO-ATTACH` transport layer).
//!
//! Ports `apps/web/src/lib/attachmentUploadState.ts`,
//! `apps/web/src/lib/attachmentUploadQueue.ts`, and
//! `packages/client-runtime/src/state/attachments.ts`.

use std::collections::{HashMap, VecDeque};

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
        /// cleaned up on retry or release. Never holds a persisted attachment ID.
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

/// Returns whether staged attachments (images and files alike) block sending to `environment_id`
/// (`attachmentUploadBlockReason` in `attachmentUploadState.ts`).
///
/// Failed uploads targeting `environment_id` do NOT block send because send retries them; missing
/// uploads, active uploads, or uploads ready/failed on a different environment count as pending.
pub fn rich_upload_block_reason<'a>(
    environment_id: &str,
    uploads: impl IntoIterator<Item = Option<&'a AttachmentUploadState>>,
) -> Option<&'static str> {
    let mut pending = 0usize;
    for upload in uploads {
        match upload {
            Some(AttachmentUploadState::Failed {
                environment_id: env,
                ..
            }) if env == environment_id => continue,
            Some(AttachmentUploadState::Ready(ready)) if ready.environment_id == environment_id => {
                continue;
            }
            _ => pending += 1,
        }
    }
    match pending {
        0 => None,
        1 => Some("Attachment still uploading"),
        _ => Some("Attachments still uploading"),
    }
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
    minted_attachment_id: Option<String>,
    restore_on_cancel: Option<ReadyAttachmentUpload>,
    state: AttachmentUploadState,
    phase: JobPhase,
    last_progress_step: i32,
    waiting_for_reconnect: bool,
    reconnect_retry_when_settled: bool,
}

#[derive(Debug, Clone)]
struct RetiredJob {
    environment_id: String,
    deleted_mint_id: Option<String>,
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
    retired_jobs: HashMap<JobToken, RetiredJob>,
    env_connected: HashMap<String, bool>,
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

    pub fn target_of(&self, job_key: &str) -> Option<&DraftTarget> {
        self.jobs.get(job_key).map(|job| &job.target)
    }

    /// Marks an already-`Ready` upload as `Failed` after send-time verification reported it
    /// missing or unreachable without local bytes to re-upload (`pingdotgg/t3code#32910`).
    pub fn mark_verification_failed(
        &mut self,
        job_key: &str,
        expected_attachment_id: &str,
        reason: String,
    ) -> Vec<QueueEffect> {
        let Some(job) = self.jobs.get_mut(job_key) else {
            return Vec::new();
        };
        let AttachmentUploadState::Ready(ref ready) = job.state else {
            return Vec::new();
        };
        if ready.attachment_id != expected_attachment_id {
            return Vec::new();
        }
        let previous = ready.clone();
        let environment_id = previous.environment_id.clone();
        let failed = AttachmentUploadState::Failed {
            environment_id: environment_id.clone(),
            attachment_id: None,
            reason,
            previous: Some(previous),
        };
        job.state = failed.clone();
        job.phase = JobPhase::Settled;
        vec![
            QueueEffect::StateChanged {
                job_key: job_key.to_owned(),
                target: job.target.clone(),
                state: Some(failed),
            },
            QueueEffect::Settled {
                job_key: job_key.to_owned(),
                environment_id,
            },
        ]
    }

    /// Records a newly minted `attachment_id` from `on_minted`. Returns `false` if `job_token` is
    /// no longer active (was cancelled or superseded while minting), signalling the caller to
    /// immediately delete the freshly minted asset.
    pub fn record_minted(&mut self, job_token: JobToken, attachment_id: String) -> bool {
        if let Some(job) = self
            .jobs
            .values_mut()
            .find(|job| job.token == job_token && job.phase == JobPhase::Active)
        {
            job.minted_attachment_id = Some(attachment_id);
            true
        } else {
            false
        }
    }

    /// Starts or adopts an upload for `job_key`.
    ///
    /// Matches `startAttachmentUpload` in `attachmentUploadQueue.ts`:
    /// - If a live job (`Queued` or `Active`) already owns the same `environment_id`, rebinds
    ///   `target` and returns its existing token.
    /// - If the file is already `Ready` or `Failed` on `environment_id`, short-circuits without
    ///   re-uploading or deleting failed mints (failed jobs stay failed until explicit retry or
    ///   reconnect).
    /// - If an in-flight/failed job on another environment carries `previous` pointing back to
    ///   `environment_id`, cancels the replacement cleanly and restores `previous` immediately.
    pub fn start(
        &mut self,
        job_key: String,
        target: DraftTarget,
        environment_id: String,
        has_bytes: bool,
        persisted_attachment_id: Option<String>,
        current_state: Option<AttachmentUploadState>,
    ) -> (Option<JobToken>, Vec<QueueEffect>) {
        self.start_with_force(
            job_key,
            target,
            environment_id,
            has_bytes,
            persisted_attachment_id,
            current_state,
            false,
        )
    }

    /// Starts or rebinds an upload; when `force` is true, bypasses an already-`Ready` upload and
    /// skips persisted-ID verification so expired server uploads re-upload fresh bytes
    /// (`pingdotgg/t3code#32910`).
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_force(
        &mut self,
        job_key: String,
        target: DraftTarget,
        environment_id: String,
        has_bytes: bool,
        persisted_attachment_id: Option<String>,
        current_state: Option<AttachmentUploadState>,
        force: bool,
    ) -> (Option<JobToken>, Vec<QueueEffect>) {
        let persisted_attachment_id = if force { None } else { persisted_attachment_id };
        if let Some(existing_job) = self.jobs.get_mut(&job_key)
            && matches!(existing_job.phase, JobPhase::Queued | JobPhase::Active)
            && existing_job.environment_id == environment_id
            && (!has_bytes || existing_job.has_bytes || persisted_attachment_id.is_none())
        {
            existing_job.target = target;
            return (Some(existing_job.token), Vec::new());
        }

        let effective_state = if force {
            None
        } else {
            current_state.or_else(|| self.jobs.get(&job_key).map(|j| j.state.clone()))
        };

        if let Some(AttachmentUploadState::Ready(ref ready)) = effective_state
            && ready.environment_id == environment_id
        {
            if let Some(job) = self.jobs.get_mut(&job_key) {
                job.target = target;
            }
            return (None, Vec::new());
        }

        if let Some(AttachmentUploadState::Failed {
            environment_id: ref fail_env,
            ..
        }) = effective_state
            && fail_env == &environment_id
        {
            if let Some(job) = self.jobs.get_mut(&job_key) {
                job.target = target;
            }
            return (None, Vec::new());
        }

        if let Some(ref state) = effective_state
            && let Some(prev) = state.previous_ready()
            && prev.environment_id == environment_id
        {
            let prev = prev.clone();
            let mut effects = self.cancel_internal(&job_key, true, false);
            if let AttachmentUploadState::Failed {
                environment_id: fail_env,
                attachment_id: Some(failed_id),
                ..
            } = state
            {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: fail_env.clone(),
                    attachment_id: failed_id.clone(),
                });
            }
            let restored = AttachmentUploadState::Ready(prev.clone());
            self.jobs.insert(
                job_key.clone(),
                QueueJob {
                    token: 0,
                    job_key: job_key.clone(),
                    environment_id,
                    target: target.clone(),
                    has_bytes,
                    persisted_attachment_id: None,
                    minted_attachment_id: None,
                    restore_on_cancel: None,
                    state: restored.clone(),
                    phase: JobPhase::Settled,
                    last_progress_step: PROGRESS_STEP_BUCKETS,
                    waiting_for_reconnect: false,
                    reconnect_retry_when_settled: false,
                },
            );
            effects.push(QueueEffect::StateChanged {
                job_key,
                target,
                state: Some(restored),
            });
            effects.extend(self.pump());
            return (None, effects);
        }

        let mut effects = Vec::new();
        let mut live_prev = None;
        if self.jobs.contains_key(&job_key) {
            let existing_is_live = self
                .jobs
                .get(&job_key)
                .is_some_and(|j| matches!(j.phase, JobPhase::Queued | JobPhase::Active));
            if existing_is_live {
                live_prev = self
                    .jobs
                    .get(&job_key)
                    .and_then(|j| j.restore_on_cancel.clone());
                effects.extend(self.cancel_internal(&job_key, false, false));
                // Removing via cancel_internal with restore_previous=false would schedule
                // deletion of live_prev; filter that out since we are preserving live_prev.
                if let Some(ref preserved) = live_prev {
                    effects.retain(|eff| {
                        !matches!(
                            eff,
                            QueueEffect::DeletePendingAttachment { environment_id, attachment_id }
                                if environment_id == &preserved.environment_id
                                    && attachment_id == &preserved.attachment_id
                        )
                    });
                }
            } else if let Some(AttachmentUploadState::Failed {
                environment_id: prev_env,
                attachment_id: Some(failed_id),
                ..
            }) = effective_state.as_ref()
            {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: prev_env.clone(),
                    attachment_id: failed_id.clone(),
                });
            }
        }

        if !has_bytes && persisted_attachment_id.is_none() {
            effects.push(QueueEffect::MarkMissingPersistedFile {
                job_key: job_key.clone(),
                target: target.clone(),
                reason: "File bytes are missing and no persisted attachment ID is available."
                    .to_owned(),
            });
            effects.push(QueueEffect::StateChanged {
                job_key,
                target,
                state: None,
            });
            return (None, effects);
        }

        let previous = live_prev.or_else(|| {
            effective_state
                .as_ref()
                .and_then(|s| s.ready_attachment().or_else(|| s.previous_ready()))
                .cloned()
        });

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
            minted_attachment_id: None,
            restore_on_cancel,
            state: state.clone(),
            phase: JobPhase::Queued,
            last_progress_step: 0,
            waiting_for_reconnect: false,
            reconnect_retry_when_settled: false,
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
    pub fn complete_persisted_verification(
        &mut self,
        job_token: JobToken,
        result: PersistedAttachmentVerification,
    ) -> (bool, Vec<QueueEffect>) {
        let mut effects = Vec::new();
        if self.retired_jobs.remove(&job_token).is_some() {
            return (false, effects);
        }

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
                    && (prev.attachment_id != attachment_id
                        || prev.environment_id != job.environment_id)
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
                job.reconnect_retry_when_settled = false;
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
                let env_id = job.environment_id.clone();
                let target = job.target.clone();
                let retry_after_settle = job.reconnect_retry_when_settled
                    && self.env_connected.get(&env_id).copied().unwrap_or(true);
                job.reconnect_retry_when_settled = false;
                // Transient verification error must never treat persisted_attachment_id as a failed mint.
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
                    job_key: key.clone(),
                    environment_id: env_id,
                });
                if retry_after_settle {
                    let (_, retry_effects) = self.retry(&key);
                    effects.extend(retry_effects);
                } else {
                    effects.extend(self.pump());
                }
                (false, effects)
            }
        }
    }

    /// Completes `runAttachmentUploadCycle` for `job_token`.
    ///
    /// If `job_token` was cancelled/superseded while active (tracked in `retired_jobs`), any
    /// newly returned `attachment_id` not already cleaned up on cancel is deleted immediately.
    pub fn complete_cycle(&mut self, job_token: JobToken, outcome: UploadCycleOutcome) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        if let Some(retired) = self.retired_jobs.remove(&job_token) {
            let returned_id = match outcome {
                UploadCycleOutcome::Uploaded { attachment_id }
                | UploadCycleOutcome::Cancelled {
                    minted_attachment_id: Some(attachment_id),
                }
                | UploadCycleOutcome::Failed {
                    attachment_id: Some(attachment_id),
                    ..
                } => Some(attachment_id),
                _ => None,
            };
            if let Some(attachment_id) = returned_id
                && retired.deleted_mint_id.as_deref() != Some(attachment_id.as_str())
            {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: retired.environment_id,
                    attachment_id,
                });
            }
            return effects;
        }

        let Some(key) = self
            .jobs
            .iter()
            .find(|(_, job)| job.token == job_token && job.phase == JobPhase::Active)
            .map(|(k, _)| k.clone())
        else {
            return effects;
        };

        let Some(job) = self.jobs.get_mut(&key) else {
            return effects;
        };

        let env_id = job.environment_id.clone();
        let target = job.target.clone();
        job.minted_attachment_id = None;

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
                job.reconnect_retry_when_settled = false;
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
                let retry_after_settle = job.reconnect_retry_when_settled
                    && self.env_connected.get(&env_id).copied().unwrap_or(true);
                job.reconnect_retry_when_settled = false;

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
                    job_key: key.clone(),
                    environment_id: env_id,
                });

                if retry_after_settle {
                    let (_, retry_effects) = self.retry(&key);
                    effects.extend(retry_effects);
                    return effects;
                }
            }
        }

        effects.extend(self.pump());
        effects
    }

    /// Cancels an upload job. When `restore_previous` is `true`, any prior `Ready` upload is
    /// restored into `StateChanged` rather than deleted; otherwise any failed mint, persisted
    /// verification asset, or ready attachment is deleted (`release`).
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
        let Some(mut job) = self.jobs.remove(job_key) else {
            return effects;
        };

        if job.phase == JobPhase::Active {
            let deleted_mint_id = job.minted_attachment_id.take();
            if let Some(ref minted) = deleted_mint_id {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: job.environment_id.clone(),
                    attachment_id: minted.clone(),
                });
            }
            self.retired_jobs.insert(
                job.token,
                RetiredJob {
                    environment_id: job.environment_id.clone(),
                    deleted_mint_id,
                },
            );
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
            if matches!(job.state, AttachmentUploadState::Uploading { .. })
                && let Some(persisted_id) = job.persisted_attachment_id.take()
            {
                effects.push(QueueEffect::DeletePendingAttachment {
                    environment_id: job.environment_id.clone(),
                    attachment_id: persisted_id,
                });
            }
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

    /// Retires a sent attachment job from queue tracking without deleting its server asset.
    pub fn retire(&mut self, job_key: &str) -> Vec<QueueEffect> {
        let mut effects = Vec::new();
        self.pending_order.retain(|k| k != job_key);
        let Some(job) = self.jobs.remove(job_key) else {
            return effects;
        };
        if job.phase == JobPhase::Active {
            self.retired_jobs.insert(
                job.token,
                RetiredJob {
                    environment_id: job.environment_id.clone(),
                    deleted_mint_id: None,
                },
            );
            effects.push(QueueEffect::AbortActive {
                job_token: job.token,
                job_key: job.job_key.clone(),
                environment_id: job.environment_id.clone(),
            });
            effects.push(QueueEffect::Settled {
                job_key: job.job_key,
                environment_id: job.environment_id,
            });
            effects.extend(self.pump());
        }
        effects
    }

    /// Moves/adopts a live or settled attachment job to a new `DraftTarget` without aborting the
    /// upload or deleting its server asset.
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
    /// minted `attachment_id` first and preserving `previous`.
    pub fn retry(&mut self, job_key: &str) -> (Option<JobToken>, Vec<QueueEffect>) {
        let Some(existing) = self.jobs.get(job_key).cloned() else {
            return (None, Vec::new());
        };
        if matches!(existing.phase, JobPhase::Queued | JobPhase::Active) {
            return (Some(existing.token), Vec::new());
        }
        let previous = existing.state.previous_ready().cloned();
        let previous_state = previous.map(AttachmentUploadState::Ready);
        let mut effects = Vec::new();
        if let AttachmentUploadState::Failed {
            environment_id: fail_env,
            attachment_id: Some(failed_id),
            ..
        } = &existing.state
        {
            effects.push(QueueEffect::DeletePendingAttachment {
                environment_id: fail_env.clone(),
                attachment_id: failed_id.clone(),
            });
        }
        self.jobs.remove(job_key);
        let (token, mut start_effects) = self.start(
            job_key.to_owned(),
            existing.target,
            existing.environment_id,
            existing.has_bytes,
            existing.persisted_attachment_id,
            previous_state,
        );
        effects.append(&mut start_effects);
        (token, effects)
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

    /// Reconnect hook matching `attachmentUploadQueue.ts:464-478`:
    /// - Retries already-settled `Failed` jobs for `environment_id` on `!was_connected && is_connected`.
    /// - Marks active/queued jobs for `environment_id` with `reconnect_retry_when_settled` so if an
    ///   in-flight attempt from the stale connection fails after reconnect, it immediately retries
    ///   upon settling (unless the environment disconnects again before settlement).
    pub fn on_connection_transition(
        &mut self,
        environment_id: &str,
        was_connected: bool,
        is_connected: bool,
    ) -> Vec<QueueEffect> {
        self.env_connected
            .insert(environment_id.to_owned(), is_connected);

        if !is_connected {
            for job in self.jobs.values_mut() {
                if job.environment_id == environment_id {
                    job.reconnect_retry_when_settled = false;
                }
            }
            return Vec::new();
        }
        if was_connected {
            return Vec::new();
        }

        let mut keys_to_retry = Vec::new();
        for (key, job) in &mut self.jobs {
            if job.environment_id != environment_id {
                continue;
            }
            if matches!(job.phase, JobPhase::Queued | JobPhase::Active) {
                job.reconnect_retry_when_settled = true;
            } else if job.phase == JobPhase::Settled
                && job.waiting_for_reconnect
                && matches!(job.state, AttachmentUploadState::Failed { .. })
            {
                keys_to_retry.push(key.clone());
            }
        }

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
    fn formats_quantizes_and_checks_all_attachments_by_environment() {
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
        let ready_a = AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: "env-a".into(),
            attachment_id: "att-1".into(),
        });
        let ready_b = AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: "env-b".into(),
            attachment_id: "att-2".into(),
        });

        assert_eq!(
            rich_upload_block_reason("env-a", [Some(&uploading)]),
            Some("Attachment still uploading")
        );
        assert_eq!(
            rich_upload_block_reason("env-a", [Some(&uploading), None]),
            Some("Attachments still uploading")
        );
        // Ready/Failed on a different environment still block target environment.
        assert_eq!(
            rich_upload_block_reason("env-a", [Some(&ready_b)]),
            Some("Attachment still uploading")
        );
        // Failed and Ready on target environment do not block send.
        assert_eq!(
            rich_upload_block_reason("env-a", [Some(&failed), Some(&ready_a)]),
            None
        );
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

        assert!(queue.update_progress(tokens_a[0], 0.02).is_empty());
        assert_eq!(queue.update_progress(tokens_a[0], 0.05).len(), 1);
        assert!(queue.update_progress(tokens_a[0], 0.08).is_empty());

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
    fn cancel_and_release_never_leak_mints_or_in_flight_persisted_uploads() {
        let mut queue = UploadQueue::new();

        // Cancel during cycle after HTTP transfer already succeeded before complete_cycle arrives.
        let (tok, _) = queue.start("race".into(), target("d1", "f1"), "env-1".into(), true, None, None);
        let tok = tok.unwrap();
        assert!(queue.record_minted(tok, "mint-raced".into()));
        let cancel_eff = queue.cancel("race", false);
        assert!(cancel_eff.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "mint-raced"
        )));
        // Late completion reporting same minted ID does not duplicate delete, whereas a new
        // unexpected uploaded ID would still be cleaned up.
        assert!(
            queue
                .complete_cycle(
                    tok,
                    UploadCycleOutcome::Uploaded {
                        attachment_id: "mint-raced".into(),
                    },
                )
                .is_empty()
        );

        // Late completion after cancel before on_minted recorded the ID still deletes it.
        let (tok2, _) = queue.start("race2".into(), target("d1", "f2"), "env-1".into(), true, None, None);
        let tok2 = tok2.unwrap();
        let _ = queue.cancel("race2", false);
        let late_eff = queue.complete_cycle(
            tok2,
            UploadCycleOutcome::Uploaded {
                attachment_id: "late-mint".into(),
            },
        );
        assert!(late_eff.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "late-mint"
        )));

        // Releasing while persisted verification is in flight deletes the persisted server asset.
        let (tok3, _) = queue.start(
            "persisted-release".into(),
            target("d1", "f3"),
            "env-1".into(),
            false,
            Some("persisted-in-flight".into()),
            None,
        );
        let _ = tok3.unwrap();
        let rel_eff = queue.release("persisted-release");
        assert!(rel_eff.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "persisted-in-flight"
        )));
    }

    #[test]
    fn start_short_circuits_ready_failed_and_retarget_roundtrip_to_previous() {
        let mut queue = UploadQueue::new();
        let (tok, _) = queue.start("j1".into(), target("d1", "f1"), "env-a".into(), true, None, None);
        let _ = queue.complete_cycle(
            tok.unwrap(),
            UploadCycleOutcome::Uploaded {
                attachment_id: "att-a".into(),
            },
        );

        // Calling start again for already Ready on env-a no-ops.
        let (dup_tok, dup_eff) = queue.start("j1".into(), target("d1", "f1"), "env-a".into(), true, None, None);
        assert!(dup_tok.is_none());
        assert!(dup_eff.is_empty());

        // Retarget to env-b (in flight), then retarget BACK to env-a restores att-a immediately.
        let (tok_b, _) = queue.start("j1".into(), target("d1", "f1"), "env-b".into(), true, None, None);
        assert!(tok_b.is_some());
        let (back_tok, back_eff) = queue.start("j1".into(), target("d1", "f1"), "env-a".into(), true, None, None);
        assert!(back_tok.is_none());
        assert!(back_eff.iter().any(|e| matches!(
            e,
            QueueEffect::AbortActive { environment_id, .. } if environment_id == "env-b"
        )));
        assert_eq!(
            queue.state("j1"),
            Some(&AttachmentUploadState::Ready(ReadyAttachmentUpload {
                environment_id: "env-a".into(),
                attachment_id: "att-a".into(),
            }))
        );

        // Start for an already-Failed job on the same env stays Failed until explicit retry.
        let (tok_f, _) = queue.start("jf".into(), target("d1", "ff"), "env-c".into(), true, None, None);
        let _ = queue.complete_cycle(
            tok_f.unwrap(),
            UploadCycleOutcome::Failed {
                step: UploadCycleStep::UploadBytes,
                attachment_id: Some("mint-f".into()),
                error: "net".into(),
            },
        );
        let (noop_f, noop_eff) = queue.start("jf".into(), target("d1", "ff"), "env-c".into(), true, None, None);
        assert!(noop_f.is_none());
        assert!(noop_eff.is_empty());
        assert!(matches!(queue.state("jf"), Some(AttachmentUploadState::Failed { .. })));
    }

    #[test]
    fn reconnect_retries_both_settled_failed_and_in_flight_failures() {
        let mut queue = UploadQueue::new();

        // Settled failure retries on disconnect->connect edge.
        let (tok, _) = queue.start("j-settled".into(), target("d1", "f1"), "env-1".into(), true, None, None);
        let _ = queue.complete_cycle(
            tok.unwrap(),
            UploadCycleOutcome::Failed {
                step: UploadCycleStep::UploadBytes,
                attachment_id: Some("mint-1".into()),
                error: "dropped".into(),
            },
        );
        let reconnect_eff = queue.on_connection_transition("env-1", false, true);
        assert!(reconnect_eff.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "mint-1"
        )));
        assert!(reconnect_eff.iter().any(|e| matches!(
            e,
            QueueEffect::StartUpload { job_key, .. } if job_key == "j-settled"
        )));

        // Reconnect arrives while another attempt on env-1 is STILL active; when it then fails,
        // it retries immediately instead of staying stuck in Failed.
        let (tok2, _) = queue.start("j-inflight".into(), target("d1", "f2"), "env-1".into(), true, None, None);
        let tok2 = tok2.unwrap();
        let _ = queue.on_connection_transition("env-1", false, true);
        let fail_then_retry = queue.complete_cycle(
            tok2,
            UploadCycleOutcome::Failed {
                step: UploadCycleStep::UploadBytes,
                attachment_id: Some("mint-2".into()),
                error: "stale conn".into(),
            },
        );
        assert!(fail_then_retry.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "mint-2"
        )));
        assert!(fail_then_retry.iter().any(|e| matches!(
            e,
            QueueEffect::StartUpload { job_key, .. } if job_key == "j-inflight"
        )));
    }

    #[test]
    fn retire_removes_ready_job_without_deleting_server_asset_or_sibling_jobs() {
        let mut queue = UploadQueue::new();
        let (tok_sent, _) = queue.start(
            "d1::sent".into(),
            target("d1", "sent"),
            "env-1".into(),
            true,
            None,
            None,
        );
        let _ = queue.complete_cycle(
            tok_sent.unwrap(),
            UploadCycleOutcome::Uploaded {
                attachment_id: "srv-sent".into(),
            },
        );
        let (_tok_keep, _) = queue.start(
            "d1::keep".into(),
            target("d1", "keep"),
            "env-1".into(),
            true,
            None,
            None,
        );

        let retire_effects = queue.retire("d1::sent");
        assert!(retire_effects.is_empty(), "retire must not delete sent server asset");
        assert!(queue.state("d1::sent").is_none());
        assert!(queue.state("d1::keep").is_some());

        // Later draft release deletes only the remaining unsent job, never srv-sent.
        let release_effects = queue.release_draft("d1");
        assert!(!release_effects.iter().any(|e| matches!(
            e,
            QueueEffect::DeletePendingAttachment { attachment_id, .. } if attachment_id == "srv-sent"
        )));
    }
}
