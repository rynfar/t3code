//! In-place file editing save coordinator, session store, revision hashing, and dismissal guard.
//! Ports:
//! - `apps/web/src/components/files/fileContentRevision.ts`
//! - `apps/web/src/components/files/fileSaveCoordinator.ts`
//! - `apps/web/src/components/files/fileSaveSessions.ts`
//! - `apps/web/src/components/files/fileEditorDismissal.ts`
//! - `apps/web/src/components/files/projectFilesQueryState.ts` (optimistic unsaved file state)

use std::collections::HashMap;

/// Default autosave debounce window (`fileSaveSessions.ts`).
pub const DEFAULT_SAVE_DEBOUNCE_MS: u64 = 500;

/// FNV-1a 32-bit revision hash over UTF-16 code units, matching `fileContentRevision.ts`:
/// `"<utf16_len>:<hash_base36>"`.
pub fn file_content_revision(contents: &str) -> String {
    let mut hash: u32 = 2_166_136_261;
    let mut utf16_len: usize = 0;
    for unit in contents.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(16_777_619);
        utf16_len += 1;
    }
    format!("{utf16_len}:{}", format_base36(hash))
}

fn format_base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut buf = [0u8; 8];
    let mut len = 0;
    while value > 0 {
        buf[len] = DIGITS[(value % 36) as usize];
        value /= 36;
        len += 1;
    }
    buf[..len].reverse();
    String::from_utf8(buf[..len].to_vec()).unwrap_or_else(|_| "0".to_owned())
}

/// `projectFileCacheKey` (`fileContentRevision.ts`).
pub fn project_file_cache_key(cwd: &str, relative_path: &str, contents: &str) -> String {
    format!("{cwd}:{relative_path}:{}", file_content_revision(contents))
}

/// Identifies one editable workspace file on one server (`FileSaveTarget`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileSaveTarget {
    pub environment_id: String,
    pub cwd: String,
    pub relative_path: String,
}

impl FileSaveTarget {
    pub fn new(
        environment_id: impl Into<String>,
        cwd: impl Into<String>,
        relative_path: impl Into<String>,
    ) -> Self {
        Self {
            environment_id: environment_id.into(),
            cwd: cwd.into(),
            relative_path: relative_path.into(),
        }
    }

    pub fn key(&self) -> String {
        format!("{}\0{}\0{}", self.environment_id, self.cwd, self.relative_path)
    }
}

/// Pure side-effect commands emitted by [`FileSaveCoordinator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveEffect {
    ScheduleDebounce { delay_ms: u64, token: u64 },
    CancelDebounce,
    PendingChanged(bool),
    Persist { revision: u64, contents: String },
    Settled,
}

/// Single-file ordered write queue (`fileSaveCoordinator.ts`). Pure state machine: no timers or I/O.
#[derive(Debug, Clone)]
pub struct FileSaveCoordinator {
    debounce_ms: u64,
    latest_contents: String,
    latest_revision: u64,
    confirmed_revision: u64,
    last_change_at_ms: u64,
    saving: bool,
    disposed: bool,
    flushing: bool,
    debounce_token: u64,
    timer_active: bool,
}

impl FileSaveCoordinator {
    pub fn new(debounce_ms: u64) -> Self {
        Self {
            debounce_ms,
            latest_contents: String::new(),
            latest_revision: 0,
            confirmed_revision: 0,
            last_change_at_ms: 0,
            saving: false,
            disposed: false,
            flushing: false,
            debounce_token: 0,
            timer_active: false,
        }
    }

    /// Record a new draft edit. Ignored after disposal.
    pub fn change(&mut self, contents: String, now_ms: u64) -> Vec<SaveEffect> {
        if self.disposed {
            return Vec::new();
        }
        self.latest_contents = contents;
        self.latest_revision += 1;
        self.last_change_at_ms = now_ms;
        let mut effects = vec![SaveEffect::PendingChanged(true)];
        self.schedule_into(self.debounce_ms, &mut effects);
        effects
    }

    /// The newest draft not yet confirmed written (`unconfirmedContents`).
    pub fn unconfirmed_contents(&self) -> Option<&str> {
        (self.latest_revision != self.confirmed_revision).then_some(self.latest_contents.as_str())
    }

    pub fn is_dirty(&self) -> bool {
        self.latest_revision != self.confirmed_revision
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed
    }

    pub fn latest_revision(&self) -> u64 {
        self.latest_revision
    }

    pub fn confirmed_revision(&self) -> u64 {
        self.confirmed_revision
    }

    /// Fires when a previously scheduled debounce token elapses. Stale tokens are ignored.
    pub fn on_debounce_fired(&mut self, token: u64, can_persist: bool) -> Vec<SaveEffect> {
        if !self.timer_active || token != self.debounce_token {
            return Vec::new();
        }
        self.timer_active = false;
        let mut effects = Vec::new();
        self.persist_latest_into(can_persist, &mut effects);
        effects
    }

    /// Writes the newest draft now instead of waiting for the debounce, then any queued behind it.
    pub fn flush(&mut self, can_persist: bool) -> Vec<SaveEffect> {
        let mut effects = Vec::new();
        self.clear_timer_into(&mut effects);
        self.flushing = true;
        self.persist_latest_into(can_persist, &mut effects);
        effects
    }

    /// Disposes the coordinator, flushing any edit if one was ever made.
    pub fn dispose(&mut self, can_persist: bool) -> Vec<SaveEffect> {
        self.disposed = true;
        let mut effects = Vec::new();
        self.clear_timer_into(&mut effects);
        if self.latest_revision > 0 {
            self.persist_latest_into(can_persist, &mut effects);
        }
        effects
    }

    /// Completes the in-flight persist call for `revision`.
    /// `confirmed` mirrors `onConfirmed(contents) !== false` (false when another editor has newer unsaved text).
    pub fn on_persist_result(
        &mut self,
        revision: u64,
        succeeded: bool,
        confirmed: bool,
        can_persist: bool,
        now_ms: u64,
    ) -> Vec<SaveEffect> {
        if !self.saving {
            return Vec::new();
        }
        let mut was_confirmed = false;
        if succeeded {
            self.confirmed_revision = revision;
            was_confirmed = confirmed;
        }
        self.saving = false;

        let mut effects = Vec::new();
        if revision == self.latest_revision {
            if was_confirmed {
                effects.push(SaveEffect::PendingChanged(false));
            }
            self.settle_into(&mut effects);
            return effects;
        }

        let elapsed = now_ms.saturating_sub(self.last_change_at_ms);
        let remaining_debounce = self.debounce_ms.saturating_sub(elapsed);
        if self.disposed || self.flushing {
            self.persist_latest_into(can_persist, &mut effects);
        } else {
            self.schedule_into(remaining_debounce, &mut effects);
        }
        effects
    }

    fn schedule_into(&mut self, delay_ms: u64, effects: &mut Vec<SaveEffect>) {
        self.clear_timer_into(effects);
        self.debounce_token += 1;
        self.timer_active = true;
        effects.push(SaveEffect::ScheduleDebounce { delay_ms, token: self.debounce_token });
    }

    fn clear_timer_into(&mut self, effects: &mut Vec<SaveEffect>) {
        if self.timer_active {
            self.timer_active = false;
            effects.push(SaveEffect::CancelDebounce);
        }
    }

    fn persist_latest_into(&mut self, can_persist: bool, effects: &mut Vec<SaveEffect>) {
        if self.saving {
            return;
        }
        if self.latest_revision == self.confirmed_revision || !can_persist {
            self.settle_into(effects);
            return;
        }
        self.saving = true;
        effects.push(SaveEffect::Persist {
            revision: self.latest_revision,
            contents: self.latest_contents.clone(),
        });
    }

    fn settle_into(&mut self, effects: &mut Vec<SaveEffect>) {
        self.flushing = false;
        effects.push(SaveEffect::Settled);
    }
}

/// Optimistic unsaved/confirmed file contents (`projectFilesQueryState.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimisticFile {
    pub contents: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone)]
struct FileSaveSession {
    target: FileSaveTarget,
    coordinator: FileSaveCoordinator,
    leases: usize,
    flushing_on_release: bool,
}

/// Effects routed to a specific [`FileSaveTarget`] from [`FileSaveSessionStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetedSaveEffect {
    pub target: FileSaveTarget,
    pub effect: SaveEffect,
}

/// Per-file lease-counted save sessions and optimistic unsaved storage (`fileSaveSessions.ts`).
#[derive(Debug, Default)]
pub struct FileSaveSessionStore {
    debounce_ms: u64,
    sessions: HashMap<String, FileSaveSession>,
    optimistic: HashMap<String, OptimisticFile>,
}

impl FileSaveSessionStore {
    pub fn new(debounce_ms: u64) -> Self {
        Self { debounce_ms, sessions: HashMap::new(), optimistic: HashMap::new() }
    }

    /// Acquires a lease for `target`. Recovers any unsaved optimistic draft if writable.
    pub fn acquire(
        &mut self,
        target: &FileSaveTarget,
        can_persist: bool,
        now_ms: u64,
    ) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let existed = self.sessions.contains_key(&key);
        let session = self.sessions.entry(key.clone()).or_insert_with(|| FileSaveSession {
            target: target.clone(),
            coordinator: FileSaveCoordinator::new(self.debounce_ms),
            leases: 0,
            flushing_on_release: false,
        });
        session.leases += 1;
        let mut out = Vec::new();
        if !existed {
            if can_persist
                && let Some(unsaved) = self.optimistic.get(&key).filter(|entry| !entry.confirmed).map(|entry| entry.contents.clone())
            {
                for effect in session.coordinator.change(unsaved, now_ms) {
                    out.push(TargetedSaveEffect { target: target.clone(), effect });
                }
            }
        } else if session.coordinator.unconfirmed_contents().is_some() {
            out.push(TargetedSaveEffect {
                target: target.clone(),
                effect: SaveEffect::PendingChanged(true),
            });
        }
        out
    }

    /// Releases a lease for `target`. The last lease flushes while keeping the session open until settled.
    pub fn release(&mut self, target: &FileSaveTarget, can_persist: bool) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        session.leases = session.leases.saturating_sub(1);
        if session.leases > 0 {
            return Vec::new();
        }
        session.flushing_on_release = true;
        let effects = session.coordinator.flush(can_persist);
        self.apply_post_effects(&key, target, effects, can_persist, 0)
    }

    /// Records a user edit if at least one surface holds a lease (`change` drops edits for retired surfaces).
    pub fn change(
        &mut self,
        target: &FileSaveTarget,
        contents: String,
        now_ms: u64,
    ) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let Some(session) = self.sessions.get_mut(&key).filter(|s| s.leases > 0) else {
            return Vec::new();
        };
        self.optimistic.insert(
            key,
            OptimisticFile { contents: contents.clone(), confirmed: false },
        );
        session
            .coordinator
            .change(contents, now_ms)
            .into_iter()
            .map(|effect| TargetedSaveEffect { target: target.clone(), effect })
            .collect()
    }

    /// Immediately flushes `target` (e.g. `Cmd+S`).
    pub fn flush(&mut self, target: &FileSaveTarget, can_persist: bool) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        let effects = session.coordinator.flush(can_persist);
        self.apply_post_effects(&key, target, effects, can_persist, 0)
    }

    /// Fires a scheduled debounce timer for `target`.
    pub fn on_debounce_fired(
        &mut self,
        target: &FileSaveTarget,
        token: u64,
        can_persist: bool,
    ) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        let effects = session.coordinator.on_debounce_fired(token, can_persist);
        self.apply_post_effects(&key, target, effects, can_persist, 0)
    }

    /// Delivers the `projects.writeFile` outcome for `target`.
    pub fn on_persist_result(
        &mut self,
        target: &FileSaveTarget,
        revision: u64,
        contents: &str,
        succeeded: bool,
        can_persist: bool,
        now_ms: u64,
    ) -> Vec<TargetedSaveEffect> {
        let key = target.key();
        let confirmed = if succeeded {
            self.confirm_optimistic(&key, contents)
        } else {
            false
        };
        let Some(session) = self.sessions.get_mut(&key) else {
            return Vec::new();
        };
        let effects = session
            .coordinator
            .on_persist_result(revision, succeeded, confirmed, can_persist, now_ms);
        self.apply_post_effects(&key, target, effects, can_persist, now_ms)
    }

    /// Called when the target environment regains `filesystem:write` scope (`subscribeWritable`).
    pub fn on_writable_regained(
        &mut self,
        environment_id: &str,
        now_ms: u64,
    ) -> Vec<TargetedSaveEffect> {
        let keys: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.target.environment_id == environment_id)
            .map(|(key, _)| key.clone())
            .collect();
        let mut out = Vec::new();
        for key in keys {
            let Some(unsaved) = self.optimistic.get(&key).filter(|entry| !entry.confirmed).map(|entry| entry.contents.clone()) else {
                continue;
            };
            if let Some(session) = self.sessions.get_mut(&key) {
                let target = session.target.clone();
                for effect in session.coordinator.change(unsaved, now_ms) {
                    out.push(TargetedSaveEffect { target: target.clone(), effect });
                }
            }
        }
        out
    }

    /// Optimistic text if present (unsaved or awaiting read confirmation).
    pub fn optimistic_contents(&self, target: &FileSaveTarget) -> Option<&str> {
        self.optimistic.get(&target.key()).map(|entry| entry.contents.as_str())
    }

    /// Unsaved text not yet confirmed persisted.
    pub fn unsaved_contents(&self, target: &FileSaveTarget) -> Option<&str> {
        self.optimistic
            .get(&target.key())
            .filter(|entry| !entry.confirmed)
            .map(|entry| entry.contents.as_str())
    }

    /// Discards optimistic unsaved changes and resets the session's dirty state for `target`.
    pub fn discard_unsaved(&mut self, target: &FileSaveTarget) {
        let key = target.key();
        self.optimistic.remove(&key);
        if let Some(session) = self.sessions.get_mut(&key) {
            session.coordinator = FileSaveCoordinator::new(self.debounce_ms);
            session.flushing_on_release = false;
        }
    }

    /// Clears a confirmed optimistic entry once a fresh server read matches it.
    pub fn clear_confirmed_if_matching(&mut self, target: &FileSaveTarget, server_contents: &str) {
        let key = target.key();
        if self.optimistic.get(&key).is_some_and(|entry| entry.confirmed && entry.contents == server_contents) {
            self.optimistic.remove(&key);
        }
    }

    pub fn is_pending(&self, target: &FileSaveTarget) -> bool {
        let key = target.key();
        self.unsaved_contents(target).is_some()
            || self.sessions.get(&key).is_some_and(|session| session.coordinator.is_dirty() || session.coordinator.is_saving())
    }

    pub fn is_saving(&self, target: &FileSaveTarget) -> bool {
        self.sessions.get(&target.key()).is_some_and(|session| session.coordinator.is_saving())
    }

    pub fn active_session_count(&self) -> usize {
        self.sessions.len()
    }

    fn confirm_optimistic(&mut self, key: &str, contents: &str) -> bool {
        let Some(entry) = self.optimistic.get_mut(key) else {
            return false;
        };
        if entry.contents != contents {
            return false;
        }
        entry.confirmed = true;
        true
    }

    fn apply_post_effects(
        &mut self,
        key: &str,
        target: &FileSaveTarget,
        effects: Vec<SaveEffect>,
        can_persist: bool,
        now_ms: u64,
    ) -> Vec<TargetedSaveEffect> {
        let settled = effects.iter().any(|effect| matches!(effect, SaveEffect::Settled));
        let mut out: Vec<TargetedSaveEffect> = effects
            .into_iter()
            .map(|effect| TargetedSaveEffect { target: target.clone(), effect })
            .collect();
        if settled {
            let mut remove_session = false;
            let mut retry_unsaved = None;
            if let Some(session) = self.sessions.get_mut(key)
                && session.flushing_on_release
            {
                session.flushing_on_release = false;
                if session.leases == 0 {
                    remove_session = true;
                } else if session.coordinator.unconfirmed_contents().is_some() && can_persist {
                    retry_unsaved = self
                        .optimistic
                        .get(key)
                        .filter(|entry| !entry.confirmed)
                        .map(|entry| entry.contents.clone());
                }
            }
            if remove_session {
                self.sessions.remove(key);
            } else if let Some(unsaved) = retry_unsaved
                && let Some(session) = self.sessions.get_mut(key)
            {
                for effect in session.coordinator.change(unsaved, now_ms) {
                    out.push(TargetedSaveEffect { target: target.clone(), effect });
                }
            }
        }
        out
    }
}

/// Dismissal rules for an active file editor (`fileEditorDismissal.ts` + unsaved-changes guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissalOutcome {
    /// Clean or unblocked: exit editing immediately.
    DismissNow,
    /// Blocked (e.g. unsaved changes or save in flight): prompt the user to Save, Discard, or Cancel.
    PromptUnsaved,
    /// Ignored (e.g. Escape when editor is not focused, or click inside editor root).
    Ignore,
}

/// Decides whether pressing Escape should dismiss, prompt, or be ignored.
pub fn decide_escape_dismissal(editor_focused: bool, blocked_by_unsaved: bool) -> DismissalOutcome {
    if !editor_focused {
        return DismissalOutcome::Ignore;
    }
    if blocked_by_unsaved {
        DismissalOutcome::PromptUnsaved
    } else {
        DismissalOutcome::DismissNow
    }
}

/// Decides whether a pointer-down outside the editor should dismiss, prompt, or be ignored.
pub fn decide_outside_pointer_dismissal(inside_editor: bool, blocked_by_unsaved: bool) -> DismissalOutcome {
    if inside_editor {
        return DismissalOutcome::Ignore;
    }
    if blocked_by_unsaved {
        DismissalOutcome::PromptUnsaved
    } else {
        DismissalOutcome::DismissNow
    }
}

/// Decides whether leaving edit mode or switching away from an edited file can proceed directly.
pub fn decide_leave_editor(has_unsaved_changes: bool, is_saving: bool) -> DismissalOutcome {
    if has_unsaved_changes || is_saving {
        DismissalOutcome::PromptUnsaved
    } else {
        DismissalOutcome::DismissNow
    }
}
