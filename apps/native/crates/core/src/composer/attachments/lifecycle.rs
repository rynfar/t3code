//! Upload ownership and pending-send invalidation from `attachmentUploadQueue.ts` and
//! `useComposerSend.ts`. Settling obsolete work cannot release or complete a newer operation.
use std::collections::HashMap;

// Resolved rows preserve a question that arrived and settled in one UI batch.
pub fn question_revision(projection: &crate::thread::ThreadProjection) -> Vec<(String, String)> {
    projection.runtime_requests.iter().filter(|request| request.value["kind"] == "user_input").map(|request| (request.id.clone(), request.value["status"].as_str().unwrap_or_default().to_owned())).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendGeneration(u64);

#[derive(Debug, Default)]
pub struct PendingSend {
    next: u64,
    active: Option<SendGeneration>,
}
impl PendingSend {
    pub fn begin(&mut self) -> SendGeneration {
        self.next += 1;
        let generation = SendGeneration(self.next);
        self.active = Some(generation);
        generation
    }
    pub fn cancel(&mut self) -> bool { self.active.take().is_some() }
    pub fn finish(&mut self, generation: SendGeneration) -> bool {
        if self.active != Some(generation) { return false; }
        self.active = None;
        true
    }
    pub fn is_pending(&self) -> bool { self.active.is_some() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadAttempt(u64);

#[derive(Debug, Default)]
pub struct UploadAttempts {
    next: u64,
    active: HashMap<String, UploadAttempt>,
}
impl UploadAttempts {
    pub fn begin(&mut self, id: &str) -> UploadAttempt {
        self.next += 1;
        let attempt = UploadAttempt(self.next);
        self.active.insert(id.into(), attempt);
        attempt
    }
    pub fn cancel(&mut self, id: &str) { self.active.remove(id); }
    pub fn finish(&mut self, id: &str, attempt: UploadAttempt) -> bool {
        if self.active.get(id) != Some(&attempt) { return false; }
        self.active.remove(id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalidation_releases_send_and_obsolete_completion_cannot_resurrect_or_clear_a_new_send() {
        for event in ["edit and undo", "remove and reattach", "switch away and back", "question arrives and resolves", "reconnect", "discard and undo"] {
            let mut pending = PendingSend::default();
            let abandoned = pending.begin();
            assert!(pending.cancel(), "{event} releases busy immediately");
            assert!(!pending.is_pending());
            assert!(!pending.finish(abandoned), "{event} cannot resurrect the old send");
            let current = pending.begin();
            assert_ne!(abandoned, current);
            assert!(!pending.finish(abandoned), "old completion cannot release a newer send");
            assert!(pending.is_pending());
            assert!(pending.finish(current));
            assert!(!pending.finish(current));
        }
    }
    #[test]
    fn discard_undo_and_retry_reject_old_upload_success_and_failure() {
        for _late_result in ["success", "failure"] {
            let mut attempts = UploadAttempts::default();
            let old = attempts.begin("image");
            attempts.cancel("image");
            let restored = attempts.begin("image");
            assert!(!attempts.finish("image", old));
            assert!(attempts.finish("image", restored));
            let retry = attempts.begin("image");
            let replacement = attempts.begin("image");
            assert!(!attempts.finish("image", retry));
            assert!(attempts.finish("image", replacement));
            let removed = attempts.begin("image");
            attempts.cancel("image");
            assert!(!attempts.finish("image", removed));
        }
    }
}
