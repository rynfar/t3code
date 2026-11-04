//! Question attachment targets and readiness from `web/src/questionAttachments.ts` and
//! `usePendingThreadRequests.ts`. Each question owns its files independently of message drafts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use super::{DraftAnswer, PendingUserInput, Question};
use crate::auth::{AuthSessionState, ORCHESTRATION_OPERATE_SCOPE};
use crate::composer::attachments::{Attachment, limit_error};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuestionDraftKey {
    pub environment: String,
    pub thread: String,
    pub request: String,
    pub question: String,
}

impl QuestionDraftKey {
    pub fn storage_key(&self) -> String {
        format!(
            "question:{}",
            serde_json::to_string(self).expect("question key")
        )
    }
    pub fn parse(key: &str) -> Option<Self> {
        serde_json::from_str(key.strip_prefix("question:")?).ok()
    }
    pub fn request_key(&self) -> String {
        serde_json::to_string(&(&self.environment, &self.thread, &self.request))
            .expect("request key")
    }
    pub fn pending<'a>(
        &self,
        inputs: &'a [PendingUserInput],
    ) -> Option<(&'a PendingUserInput, &'a Question)> {
        let request = inputs
            .iter()
            .find(|input| input.request_id == self.request)?;
        Some((
            request,
            request
                .questions
                .iter()
                .find(|question| question.id == self.question)?,
        ))
    }
    pub fn resolved(&self, environment: &str, thread: &str, inputs: &[PendingUserInput]) -> bool {
        self.environment == environment && self.thread == thread && self.pending(inputs).is_none()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IntakePhase {
    Start,
    Preparing,
}

// A dismissal may fail; admitted reads survive until the request resolves.
pub fn can_attach(
    question: &Question,
    input: &PendingUserInput,
    supports: bool,
    grants: Option<&AuthSessionState>,
    responding: bool,
    phase: IntakePhase,
) -> bool {
    question.allows_custom_answer()
        && input.can_respond()
        && supports
        && (!responding || phase == IntakePhase::Preparing)
        && grants.is_some_and(|grants| grants.grants(ORCHESTRATION_OPERATE_SCOPE))
}

pub fn answer_draft(
    draft: Option<&DraftAnswer>,
    attachments: &[Attachment],
    preparing: usize,
    supports: bool,
) -> DraftAnswer {
    DraftAnswer {
        attachment_count: attachments.len(),
        attachments_blocked: preparing > 0
            || (!attachments.is_empty() && !supports)
            || attachments
                .iter()
                .any(|attachment| attachment.attachment_id().is_none())
            || limit_error(attachments).is_some(),
        ..draft.cloned().unwrap_or_default()
    }
}

/// Reservations survive navigation, but clearing a resolved draft invalidates its async reads.
pub struct Preparations<T = ()> {
    next: u64,
    active: HashMap<String, BTreeMap<u64, Option<T>>>,
}
impl<T> Default for Preparations<T> {
    fn default() -> Self {
        Self {
            next: 0,
            active: HashMap::new(),
        }
    }
}
impl<T> Preparations<T> {
    pub fn begin(&mut self, key: &str) -> u64 {
        self.next += 1;
        self.active
            .entry(key.into())
            .or_default()
            .insert(self.next, None);
        self.next
    }
    pub fn count(&self, key: &str) -> usize {
        self.active.get(key).map_or(0, BTreeMap::len)
    }
    pub fn complete(&mut self, key: &str, token: u64, value: T) -> bool {
        let Some(held) = self
            .active
            .get_mut(key)
            .and_then(|active| active.get_mut(&token))
        else {
            return false;
        };
        if held.is_some() {
            return false;
        }
        *held = Some(value);
        true
    }
    pub fn take_completed(&mut self, key: &str, eligible: bool) -> Vec<T> {
        let mut completed = Vec::new();
        if !eligible {
            return completed;
        }
        let Some(active) = self.active.get_mut(key) else {
            return completed;
        };
        active.retain(|_, value| {
            if let Some(value) = value.take() {
                completed.push(value);
                false
            } else {
                true
            }
        });
        if active.is_empty() {
            self.active.remove(key);
        }
        completed
    }
    pub fn clear(&mut self, key: &str) {
        self.active.remove(key);
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.active.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn question_keys_round_trip_without_cross_environment_thread_request_or_question_collisions() {
        let base = QuestionDraftKey {
            environment: "env:a".into(),
            thread: "t/1".into(),
            request: "r[]".into(),
            question: "q\"".into(),
        };
        assert_eq!(
            QuestionDraftKey::parse(&base.storage_key()),
            Some(base.clone())
        );
        for other in [
            QuestionDraftKey {
                environment: "other".into(),
                ..base.clone()
            },
            QuestionDraftKey {
                thread: "other".into(),
                ..base.clone()
            },
            QuestionDraftKey {
                request: "other".into(),
                ..base.clone()
            },
            QuestionDraftKey {
                question: "other".into(),
                ..base.clone()
            },
        ] {
            assert_ne!(base.storage_key(), other.storage_key());
        }
        assert!(base.resolved("env:a", "t/1", &[]));
        assert!(!base.resolved("other", "t/1", &[]));
        assert!(!base.resolved("env:a", "other", &[]));
        assert!(QuestionDraftKey::parse("thread:t/1").is_none());
    }
    #[test]
    fn intake_requires_custom_capability_permission_live_request_and_no_response_in_flight() {
        let mut q: Question =
            serde_json::from_value(json!({"id":"q", "header":"Question", "question":"?"})).unwrap();
        let mut input = PendingUserInput {
            request_id: "r".into(),
            created_at: String::new(),
            questions: vec![q.clone()],
            response_capability: "live".into(),
            dismissible: false,
        };
        let grants = serde_json::from_value(
            json!({"authenticated":true,"permissions":["orchestration:operate"]}),
        )
        .unwrap();
        assert!(can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            false,
            IntakePhase::Start
        ));
        assert!(!can_attach(
            &q,
            &input,
            false,
            Some(&grants),
            false,
            IntakePhase::Start
        ));
        assert!(!can_attach(
            &q,
            &input,
            true,
            None,
            false,
            IntakePhase::Start
        ));
        assert!(!can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            true,
            IntakePhase::Start
        ));
        assert!(can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            true,
            IntakePhase::Preparing
        ));
        assert!(!can_attach(
            &q,
            &input,
            true,
            None,
            true,
            IntakePhase::Preparing
        ));
        q.allow_custom_answer = Some(false);
        assert!(!can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            false,
            IntakePhase::Start
        ));
        assert!(!can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            true,
            IntakePhase::Preparing
        ));
        q.allow_custom_answer = None;
        input.response_capability = "not_resumable".into();
        assert!(!can_attach(
            &q,
            &input,
            true,
            Some(&grants),
            false,
            IntakePhase::Start
        ));
    }
    #[test]
    fn resolving_preparations_rejects_late_work_even_after_reopening_the_same_key() {
        let mut preparations = Preparations::default();
        let old = preparations.begin("question");
        assert_eq!(preparations.count("question"), 1);
        preparations.clear("question");
        let current = preparations.begin("question");
        assert!(!preparations.complete("question", old, "obsolete"));
        assert_eq!(preparations.count("question"), 1);
        assert!(preparations.complete("question", current, "current"));
        assert!(!preparations.complete("question", current, "duplicate"));
        assert_eq!(
            preparations.take_completed("question", true),
            vec!["current"]
        );
        assert_eq!(preparations.count("question"), 0);
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use crate::composer::attachments::{AttachmentKind, Upload};
    use serde_json::json;
    #[test]
    fn navigation_keeps_each_questions_files_and_preparation_upload_or_capability_blocks_completion()
     {
        let questions: Vec<Question> = serde_json::from_value(json!([
            {"id":"q1","header":"First","question":"?"}, {"id":"q2","header":"Second","question":"?"}
        ])).unwrap();
        let ready = Attachment {
            id: "local".into(),
            kind: AttachmentKind::File,
            name: "details.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 12,
            pasted_text: false,
            upload: Upload::Ready {
                attachment_id: "uploaded".into(),
            },
        };
        let mut preparations = Preparations::default();
        let key = QuestionDraftKey {
            environment: "env".into(),
            thread: "original".into(),
            request: "request".into(),
            question: "q1".into(),
        };
        let storage = key.storage_key();
        let read = preparations.begin(&storage);
        let mut synchronized = HashMap::from([("original".into(), vec!["request".into()])]);
        synchronized.remove("original");
        assert!(resolved_draft_keys(std::slice::from_ref(&key), "env", &synchronized).is_empty());
        assert!(preparations.complete(&storage, read, vec![ready.clone()]));
        assert!(
            preparations
                .take_completed(&storage, synchronized.contains_key("original"))
                .is_empty()
        );
        assert_eq!(
            preparations.count(&storage),
            1,
            "navigation retains the completed read and its completion guard"
        );
        let typed = DraftAnswer {
            custom: Some("first answer".into()),
            ..Default::default()
        };
        assert!(
            answer_draft(Some(&typed), &[], preparations.count(&storage), true).attachments_blocked
        );
        synchronized.insert("original".into(), vec!["request".into()]);
        assert!(resolved_draft_keys(std::slice::from_ref(&key), "env", &synchronized).is_empty());
        let input = PendingUserInput {
            request_id: "request".into(),
            created_at: String::new(),
            questions: questions.clone(),
            response_capability: "message".into(),
            dismissible: true,
        };
        let grants = serde_json::from_value(
            json!({"authenticated":true,"permissions":["orchestration:operate"]}),
        )
        .unwrap();
        let (pending, question) = key.pending(std::slice::from_ref(&input)).unwrap();
        let eligible = can_attach(
            question,
            pending,
            true,
            Some(&grants),
            true,
            IntakePhase::Preparing,
        );
        let restored: Vec<_> = preparations
            .take_completed(&storage, synchronized.contains_key("original") && eligible)
            .into_iter()
            .flatten()
            .collect();
        assert!(
            can_attach(
                question,
                pending,
                true,
                Some(&grants),
                false,
                IntakePhase::Start
            ),
            "a failed dismissal leaves the still-pending request available"
        );
        assert_eq!(restored, vec![ready.clone()]);
        assert_eq!(preparations.count(&storage), 0);
        assert_eq!(
            answer_draft(Some(&typed), &restored, 0, true)
                .custom
                .as_deref(),
            Some("first answer")
        );
        let late = preparations.begin(&storage);
        assert!(preparations.complete(&storage, late, vec![ready.clone()]));
        synchronized.insert("original".into(), Vec::new());
        for resolved in resolved_draft_keys(std::slice::from_ref(&key), "env", &synchronized) {
            preparations.clear(&resolved.storage_key());
        }
        assert!(preparations.take_completed(&storage, true).is_empty());
        assert!(!preparations.complete(&storage, late, vec![ready.clone()]));
        let mut drafts = HashMap::from([
            (
                "q1".into(),
                answer_draft(None, std::slice::from_ref(&ready), 0, true),
            ),
            (
                "q2".into(),
                DraftAnswer {
                    custom: Some("second answer".into()),
                    ..Default::default()
                },
            ),
        ]);
        let first = super::super::progress(&questions, &drafts, 0);
        let second = super::super::progress(&questions, &drafts, 1);
        assert!(first.can_advance && second.is_complete);
        assert_eq!(
            super::super::build_answers(&questions, &drafts),
            Some(serde_json::from_value(json!({"q1":"","q2":"second answer"})).unwrap())
        );
        for upload in [
            Upload::Uploading,
            Upload::Failed {
                reason: "failure".into(),
            },
        ] {
            let pending = Attachment {
                upload,
                ..ready.clone()
            };
            drafts.insert("q1".into(), answer_draft(None, &[pending], 0, true));
            assert!(!super::super::progress(&questions, &drafts, 1).is_complete);
        }
        for (preparing, supports) in [(1, true), (0, false)] {
            drafts.insert(
                "q1".into(),
                answer_draft(None, std::slice::from_ref(&ready), preparing, supports),
            );
            assert!(super::super::build_answers(&questions, &drafts).is_none());
        }
        drafts.insert("q1".into(), answer_draft(None, &[], 0, true));
        assert!(
            super::super::build_answers(&questions, &drafts).is_none(),
            "removing the last file reopens its question"
        );
    }
}

/// `resolvedQuestionDraftKeys`: only synchronized threads are authoritative, including threads
/// whose response finished after the user navigated away.
pub fn resolved_draft_keys<'a>(
    keys: &'a [QuestionDraftKey],
    environment: &str,
    pending_by_thread: &HashMap<String, Vec<String>>,
) -> Vec<&'a QuestionDraftKey> {
    keys.iter()
        .filter(|key| {
            key.environment == environment
                && pending_by_thread
                    .get(&key.thread)
                    .is_some_and(|pending| !pending.contains(&key.request))
        })
        .collect()
}

#[cfg(test)]
mod resolution_tests {
    use super::*;
    #[test]
    fn resolution_prunes_background_threads_but_never_unsynchronized_threads_or_other_environments()
    {
        let key = QuestionDraftKey {
            environment: "env".into(),
            thread: "background".into(),
            request: "resolved".into(),
            question: "q".into(),
        };
        let keys = vec![
            key.clone(),
            QuestionDraftKey {
                request: "pending".into(),
                ..key.clone()
            },
            QuestionDraftKey {
                thread: "not-loaded".into(),
                ..key.clone()
            },
            QuestionDraftKey {
                environment: "other".into(),
                ..key.clone()
            },
        ];
        let loaded = HashMap::from([("background".into(), vec!["pending".into()])]);
        assert_eq!(resolved_draft_keys(&keys, "env", &loaded), vec![&key]);
    }
}
