//! Async Tokio execution engine for `t3_core::composer::attachments::queue::UploadQueue`.
//!
//! Enforces `MAX_UPLOADS_PER_ENVIRONMENT = 3` concurrent HTTP transfers per environment, checks
//! `ORCHESTRATION_OPERATE_SCOPE` grants, forwards quantized streaming progress callbacks, reconciles
//! draft ownership/adoption, cleans up minted/persisted server assets on cancel/release, verifies
//! persisted uploads, and auto-retries failed jobs (including in-flight reconnect races) on
//! environment reconnection.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use t3_core::composer::attachments::queue::{
    AttachmentUploadState, DraftTarget, JobToken, PersistedAttachmentVerification, QueueEffect,
    UPLOAD_TIMEOUT_MS, UploadCycleOutcome, UploadCycleStep, UploadQueue,
};
use tokio::sync::{mpsc, oneshot, watch};

use super::UploadedAttachment;
use super::cycle::{
    DEFAULT_UPLOAD_CHUNK_SIZE, delete_pending_attachment, has_orchestration_operate_scope,
    run_attachment_upload_cycle, verify_persisted_attachment_upload,
};
use crate::supervisor::ClientHandle;

/// Input payload tracked by [`AttachmentUploadQueueClient`] for a queued upload or verification.
#[derive(Debug, Clone)]
pub struct UploadPayload {
    pub request: Value,
    pub mime_type: String,
    pub bytes: Option<Vec<u8>>,
    pub persisted_attachment_id: Option<String>,
    pub current_state: Option<AttachmentUploadState>,
}

/// Notifications emitted as queued uploads make progress or change state.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueEvent {
    Progress {
        job_key: String,
        target: DraftTarget,
        state: AttachmentUploadState,
    },
    StateChanged {
        job_key: String,
        target: DraftTarget,
        state: Option<AttachmentUploadState>,
    },
    MissingPersistedFile {
        job_key: String,
        target: DraftTarget,
        reason: String,
    },
}

struct QueueInner {
    queue: UploadQueue,
    clients: HashMap<String, ClientHandle>,
    watched_envs: HashSet<String>,
    payloads: HashMap<String, UploadPayload>,
    active_aborts: HashMap<JobToken, watch::Sender<bool>>,
    waiters: HashMap<String, Vec<oneshot::Sender<()>>>,
    events: mpsc::UnboundedSender<QueueEvent>,
    chunk_size: usize,
    timeout: Duration,
}

/// Concurrent per-environment upload coordinator driven by [`UploadQueue`].
#[derive(Clone)]
pub struct AttachmentUploadQueueClient {
    runtime: tokio::runtime::Handle,
    inner: Arc<Mutex<QueueInner>>,
}

impl AttachmentUploadQueueClient {
    /// Creates a new upload queue runner executing tasks on `runtime`.
    pub fn new(runtime: tokio::runtime::Handle) -> (Self, mpsc::UnboundedReceiver<QueueEvent>) {
        Self::with_options(
            runtime,
            DEFAULT_UPLOAD_CHUNK_SIZE,
            Duration::from_millis(UPLOAD_TIMEOUT_MS),
        )
    }

    /// Creates a queue runner with custom transfer chunk size and timeout (useful in tests).
    pub fn with_options(
        runtime: tokio::runtime::Handle,
        chunk_size: usize,
        timeout: Duration,
    ) -> (Self, mpsc::UnboundedReceiver<QueueEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = Self {
            runtime,
            inner: Arc::new(Mutex::new(QueueInner {
                queue: UploadQueue::new(),
                clients: HashMap::new(),
                watched_envs: HashSet::new(),
                payloads: HashMap::new(),
                active_aborts: HashMap::new(),
                waiters: HashMap::new(),
                events: tx,
                chunk_size: chunk_size.max(1),
                timeout,
            })),
        };
        (client, rx)
    }

    /// Registers `client` for its environment and attaches a reconnect watcher if one is not
    /// already active for that environment.
    pub fn register_client(&self, client: ClientHandle) {
        let env_id = client.environment().to_owned();
        let should_watch = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.clients.insert(env_id.clone(), client.clone());
            inner.watched_envs.insert(env_id.clone())
        };
        if should_watch {
            self.spawn_reconnect_watcher(env_id, client);
        }
    }

    fn spawn_reconnect_watcher(&self, env_id: String, client: ClientHandle) {
        let this = self.clone();
        let mut gen_rx = client.connection_generation();
        let mut last_seen = *gen_rx.borrow();
        drop(self.runtime.spawn(async move {
            while gen_rx.changed().await.is_ok() {
                let next = *gen_rx.borrow_and_update();
                if next > last_seen {
                    last_seen = next;
                    let latest_client = {
                        let inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                        inner.clients.get(&env_id).cloned()
                    };
                    if latest_client.as_ref().is_some_and(has_orchestration_operate_scope) {
                        this.notify_connection_transition(&env_id, false, true);
                    }
                }
            }
        }));
    }

    /// Enqueues or adopts an attachment upload under `job_key`.
    ///
    /// Matches `startAttachmentUpload`: no-ops when `client` lacks `ORCHESTRATION_OPERATE_SCOPE`.
    pub fn start_upload(
        &self,
        job_key: String,
        target: DraftTarget,
        client: ClientHandle,
        payload: UploadPayload,
    ) -> Option<JobToken> {
        if !has_orchestration_operate_scope(&client) {
            return None;
        }
        self.register_client(client.clone());
        let env_id = client.environment().to_owned();
        let persisted_id = payload.persisted_attachment_id.clone();
        let current_state = payload.current_state.clone();

        let (token, effects) = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let mut merged = payload;
            if merged.bytes.is_none()
                && let Some(prev) = inner.payloads.get(&job_key)
            {
                merged.bytes = prev.bytes.clone();
            }
            let has_bytes = merged.bytes.is_some();
            inner.payloads.insert(job_key.clone(), merged);
            inner.queue.start(
                job_key,
                target,
                env_id,
                has_bytes,
                persisted_id,
                current_state,
            )
        };
        self.apply_effects(effects);
        token
    }

    /// Retries a settled failed upload for `job_key`, gated on `ORCHESTRATION_OPERATE_SCOPE`.
    pub fn retry_upload(&self, job_key: &str) -> Option<JobToken> {
        let (token, effects) = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let env_id = inner.queue.state(job_key)?.environment_id().to_owned();
            if !inner
                .clients
                .get(&env_id)
                .is_some_and(has_orchestration_operate_scope)
            {
                return None;
            }
            inner.queue.retry(job_key)
        };
        self.apply_effects(effects);
        token
    }

    /// Retries every failed job in `job_keys` on send (`retry-on-send`), gated on
    /// `ORCHESTRATION_OPERATE_SCOPE`.
    pub fn retry_on_send(&self, job_keys: &[String]) -> Vec<JobToken> {
        let (tokens, effects) = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let permitted_keys: Vec<String> = job_keys
                .iter()
                .filter(|key| {
                    inner
                        .queue
                        .state(key)
                        .and_then(|state| inner.clients.get(state.environment_id()))
                        .is_some_and(has_orchestration_operate_scope)
                })
                .cloned()
                .collect();
            inner.queue.retry_on_send(&permitted_keys)
        };
        self.apply_effects(effects);
        tokens
    }

    /// Cancels `job_key`, optionally restoring a previously ready upload instead of deleting it.
    pub fn cancel_upload(&self, job_key: &str, restore_previous: bool) {
        let effects = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if !restore_previous {
                inner.payloads.remove(job_key);
            }
            inner.queue.cancel(job_key, restore_previous)
        };
        self.apply_effects(effects);
    }

    /// Releases an upload owned by `job_key`, deleting any staged/failed/persisted server asset.
    pub fn release_upload(&self, job_key: &str) {
        let effects = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.payloads.remove(job_key);
            inner.queue.release(job_key)
        };
        self.apply_effects(effects);
    }

    /// Explicitly deletes a persisted ready attachment ID when removed from a draft.
    pub fn release_persisted_upload(&self, client: &ClientHandle, attachment_id: &str) {
        let client = client.clone();
        let attachment_id = attachment_id.to_owned();
        drop(self.runtime.spawn(async move {
            delete_pending_attachment(&client, &attachment_id).await;
        }));
    }

    /// Releases every attachment owned by `draft_id`.
    pub fn release_draft(&self, draft_id: &str) {
        let effects = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            for key in inner.queue.job_keys_for_draft(draft_id) {
                inner.payloads.remove(&key);
            }
            inner.queue.release_draft(draft_id)
        };
        self.apply_effects(effects);
    }

    /// Retires already-sent attachment jobs (`job_keys`) without deleting their server assets.
    pub fn retire_sent(&self, job_keys: &[String]) {
        if job_keys.is_empty() {
            return;
        }
        let effects = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let mut effects = Vec::new();
            for key in job_keys {
                inner.payloads.remove(key);
                effects.extend(inner.queue.retire(key));
            }
            effects
        };
        self.apply_effects(effects);
    }

    /// Adopts an existing live or settled upload under a new `job_key` and `DraftTarget` without
    /// aborting the upload or deleting server assets.
    pub fn adopt_target(&self, old_job_key: &str, new_job_key: String, new_target: DraftTarget) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let adopted = inner
            .queue
            .adopt_target(old_job_key, new_job_key.clone(), new_target);
        if adopted {
            if let Some(payload) = inner.payloads.remove(old_job_key) {
                inner.payloads.insert(new_job_key.clone(), payload);
            }
            if let Some(waiters) = inner.waiters.remove(old_job_key) {
                inner.waiters.entry(new_job_key).or_default().extend(waiters);
            }
        }
        adopted
    }

    /// Transfers cleanup ownership of a `Ready` attachment out of the queue into an RAII
    /// [`UploadedAttachment`] handle so dropping that handle deletes it unless `.into_id()` is called.
    pub fn take_uploaded_owned(&self, job_key: &str) -> Option<UploadedAttachment> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let ready = inner.queue.state(job_key)?.ready_attachment()?.clone();
        let client = inner.clients.get(&ready.environment_id)?.clone();
        let runtime = self.runtime.clone();
        inner.payloads.remove(job_key);
        let _ = inner.queue.cancel(job_key, true);
        Some(UploadedAttachment::new(ready.attachment_id, move |id| {
            drop(runtime.spawn(async move {
                delete_pending_attachment(&client, &id).await;
            }));
        }))
    }

    /// Triggers reconnect retry handling on a `!was_connected && is_connected` edge (or records a
    /// disconnect via `is_connected == false`).
    pub fn notify_connection_transition(
        &self,
        environment_id: &str,
        was_connected: bool,
        is_connected: bool,
    ) {
        let effects = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if is_connected
                && !inner
                    .clients
                    .get(environment_id)
                    .is_some_and(has_orchestration_operate_scope)
            {
                return;
            }
            inner
                .queue
                .on_connection_transition(environment_id, was_connected, is_connected)
        };
        self.apply_effects(effects);
    }

    pub fn state(&self, job_key: &str) -> Option<AttachmentUploadState> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.queue.state(job_key).cloned()
    }

    pub fn active_count(&self, environment_id: &str) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.queue.active_count(environment_id)
    }

    pub fn queued_count(&self, environment_id: &str) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.queue.queued_count(environment_id)
    }

    /// Revalidates `Ready` uploads on `environment_id` before sending (`refreshExpiredAttachmentUploads`
    /// in `apps/web/src/lib/attachmentUploadQueue.ts`, `pingdotgg/t3code#32910`): long-lived drafts
    /// can outlive server attachment cleanup, so missing uploads with local bytes force a fresh
    /// re-upload while missing/unverifiable uploads without bytes fail clearly.
    pub async fn refresh_expired_uploads(&self, environment_id: &str, job_keys: &[String]) {
        if job_keys.is_empty() {
            return;
        }
        let mut checks = Vec::new();
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let client = inner.clients.get(environment_id).cloned();
            for key in job_keys {
                let Some(AttachmentUploadState::Ready(ready)) = inner.queue.state(key).cloned()
                else {
                    continue;
                };
                if ready.environment_id != environment_id {
                    continue;
                }
                let has_bytes = inner
                    .payloads
                    .get(key)
                    .is_some_and(|payload| payload.bytes.is_some());
                let Some(target) = inner.queue.target_of(key).cloned() else {
                    continue;
                };
                checks.push((key.clone(), ready, has_bytes, target, client.clone()));
            }
        }

        let tasks = checks.into_iter().map(|(key, ready, has_bytes, target, client)| {
            let this = self.clone();
            let env_id = environment_id.to_owned();
            async move {
                let Some(client) = client else {
                    let effects = {
                        let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                        inner.queue.mark_verification_failed(
                            &key,
                            &ready.attachment_id,
                            "Uploaded attachment could not be verified. Retry when the server reconnects."
                                .to_owned(),
                        )
                    };
                    this.apply_effects(effects);
                    return;
                };
                let can_verify = client
                    .session_state()
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| {
                        session.grants(t3_core::auth::ORCHESTRATION_READ_SCOPE)
                            || session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE)
                    });
                if !can_verify {
                    if has_bytes && has_orchestration_operate_scope(&client) {
                        let effects = {
                            let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                            inner
                                .queue
                                .start_with_force(key, target, env_id, true, None, None, true)
                                .1
                        };
                        this.apply_effects(effects);
                    } else {
                        let effects = {
                            let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                            inner.queue.mark_verification_failed(
                                &key,
                                &ready.attachment_id,
                                "Uploaded attachment could not be verified. Attach the file again."
                                    .to_owned(),
                            )
                        };
                        this.apply_effects(effects);
                    }
                    return;
                }

                let verification =
                    verify_persisted_attachment_upload(&client, &ready.attachment_id).await;
                let effects = {
                    let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                    // The user may have removed or replaced the attachment while verification ran.
                    if inner.queue.state(&key) != Some(&AttachmentUploadState::Ready(ready.clone())) {
                        return;
                    }
                    match verification {
                        PersistedAttachmentVerification::Verified => Vec::new(),
                        PersistedAttachmentVerification::Missing { .. }
                            if inner
                                .payloads
                                .get(&key)
                                .is_some_and(|payload| payload.bytes.is_some())
                                && has_orchestration_operate_scope(&client) =>
                        {
                            inner
                                .queue
                                .start_with_force(key, target, env_id, true, None, None, true)
                                .1
                        }
                        PersistedAttachmentVerification::Missing { .. } => inner
                            .queue
                            .mark_verification_failed(
                                &key,
                                &ready.attachment_id,
                                "Uploaded attachment expired. Remove it and attach it again."
                                    .to_owned(),
                            ),
                        PersistedAttachmentVerification::Failed { .. } => inner
                            .queue
                            .mark_verification_failed(
                                &key,
                                &ready.attachment_id,
                                "Uploaded attachment could not be verified. Retry when the server reconnects."
                                    .to_owned(),
                            ),
                    }
                };
                this.apply_effects(effects);
            }
        });
        futures::future::join_all(tasks).await;
        self.await_uploads(job_keys).await;
    }

    /// Awaits settlement of every pending upload named in `job_keys` without polling.
    pub async fn await_uploads(&self, job_keys: &[String]) {
        let receivers = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let mut rxs = Vec::new();
            for key in job_keys {
                if inner.queue.is_pending(key) {
                    let (tx, rx) = oneshot::channel();
                    inner.waiters.entry(key.clone()).or_default().push(tx);
                    rxs.push(rx);
                }
            }
            rxs
        };
        let _ = futures::future::join_all(receivers).await;
    }

    fn apply_effects(&self, initial_effects: Vec<QueueEffect>) {
        let mut pending_effects: std::collections::VecDeque<QueueEffect> = initial_effects.into();
        let mut tasks_to_spawn = Vec::new();
        let mut deletes_to_spawn = Vec::new();

        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            while let Some(effect) = pending_effects.pop_front() {
                match effect {
                    QueueEffect::StartUpload {
                        job_token,
                        job_key,
                        environment_id,
                        persisted_attachment_id,
                        ..
                    } => {
                        let Some(client) = inner.clients.get(&environment_id).cloned() else {
                            // Never leave an active slot wedged when no client handle is registered.
                            let follow_up = if persisted_attachment_id.is_some() {
                                inner
                                    .queue
                                    .complete_persisted_verification(
                                        job_token,
                                        PersistedAttachmentVerification::Failed {
                                            reason: "Not connected".to_owned(),
                                        },
                                    )
                                    .1
                            } else {
                                inner.queue.complete_cycle(
                                    job_token,
                                    UploadCycleOutcome::Failed {
                                        step: UploadCycleStep::ResolveUploadUrl,
                                        attachment_id: None,
                                        error: "Not connected".to_owned(),
                                    },
                                )
                            };
                            pending_effects.extend(follow_up);
                            continue;
                        };
                        let payload = inner.payloads.get(&job_key).cloned();
                        let (abort_tx, abort_rx) = watch::channel(false);
                        inner.active_aborts.insert(job_token, abort_tx);
                        tasks_to_spawn.push((
                            job_token,
                            job_key,
                            client,
                            persisted_attachment_id,
                            payload,
                            abort_rx,
                            inner.chunk_size,
                            inner.timeout,
                        ));
                    }
                    QueueEffect::AbortActive { job_token, .. } => {
                        if let Some(abort_tx) = inner.active_aborts.remove(&job_token) {
                            let _ = abort_tx.send(true);
                        }
                    }
                    QueueEffect::DeletePendingAttachment {
                        environment_id,
                        attachment_id,
                    } => {
                        if let Some(client) = inner.clients.get(&environment_id).cloned() {
                            deletes_to_spawn.push((client, attachment_id));
                        }
                    }
                    QueueEffect::NotifyProgress {
                        job_key,
                        target,
                        state,
                    } => {
                        let _ = inner.events.send(QueueEvent::Progress {
                            job_key,
                            target,
                            state,
                        });
                    }
                    QueueEffect::MarkMissingPersistedFile {
                        job_key,
                        target,
                        reason,
                    } => {
                        inner.payloads.remove(&job_key);
                        let _ = inner.events.send(QueueEvent::MissingPersistedFile {
                            job_key,
                            target,
                            reason,
                        });
                    }
                    QueueEffect::StateChanged {
                        job_key,
                        target,
                        state,
                    } => {
                        let _ = inner.events.send(QueueEvent::StateChanged {
                            job_key,
                            target,
                            state,
                        });
                    }
                    QueueEffect::Settled { job_key, .. } => {
                        if let Some(waiters) = inner.waiters.remove(&job_key) {
                            for tx in waiters {
                                let _ = tx.send(());
                            }
                        }
                    }
                }
            }
        }

        for (client, attachment_id) in deletes_to_spawn {
            drop(self.runtime.spawn(async move {
                delete_pending_attachment(&client, &attachment_id).await;
            }));
        }

        for (
            job_token,
            job_key,
            client,
            persisted_attachment_id,
            payload,
            mut abort_rx,
            chunk_size,
            timeout,
        ) in tasks_to_spawn
        {
            let this = self.clone();
            drop(self.runtime.spawn(async move {
                if let Some(persisted_id) = persisted_attachment_id {
                    let verification =
                        verify_persisted_attachment_upload(&client, &persisted_id).await;
                    let (continue_cycle, effects) = {
                        let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                        let result = inner
                            .queue
                            .complete_persisted_verification(job_token, verification);
                        if !result.0 {
                            inner.active_aborts.remove(&job_token);
                        }
                        result
                    };
                    this.apply_effects(effects);
                    if !continue_cycle {
                        return;
                    }
                }

                let outcome = match payload.and_then(|p| p.bytes.map(|b| (p.request, p.mime_type, b))) {
                    Some((request, mime_type, bytes)) => {
                        let mint_this = this.clone();
                        let mint_abort = abort_rx.clone();
                        let progress_this = this.clone();
                        run_attachment_upload_cycle(
                            &client,
                            request,
                            &mime_type,
                            &bytes,
                            chunk_size,
                            timeout,
                            &mut abort_rx,
                            |minted_id| {
                                if *mint_abort.borrow() {
                                    return false;
                                }
                                let mut inner = mint_this.inner.lock().unwrap_or_else(|p| p.into_inner());
                                inner.queue.record_minted(job_token, minted_id.to_owned())
                            },
                            |progress| {
                                let effects = {
                                    let mut inner =
                                        progress_this.inner.lock().unwrap_or_else(|p| p.into_inner());
                                    inner.queue.update_progress(job_token, progress)
                                };
                                progress_this.apply_effects(effects);
                            },
                        )
                        .await
                    }
                    None => UploadCycleOutcome::Failed {
                        step: UploadCycleStep::UploadBytes,
                        attachment_id: None,
                        error: format!("Upload payload missing for {job_key}"),
                    },
                };

                let effects = {
                    let mut inner = this.inner.lock().unwrap_or_else(|p| p.into_inner());
                    inner.active_aborts.remove(&job_token);
                    inner.queue.complete_cycle(job_token, outcome)
                };
                this.apply_effects(effects);
            }));
        }
    }
}
