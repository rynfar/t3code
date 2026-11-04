//! Per-question intake, persistence and response payloads from `questionAttachments.ts`,
//! `usePendingThreadRequests.ts` and `ChatComposer.tsx`.

use gpui::{App, Context};
use serde_json::{Map, Value};
use std::collections::HashMap;
use t3_core::auth::ORCHESTRATION_OPERATE_SCOPE;
use t3_core::composer::attachments::{self, Attachment};
use t3_core::requests::{DraftAnswer, PendingUserInput, attachments as rules};

use super::{Composer, Target, drafts};

impl Composer {
    pub(super) fn question_target(&self, request: &str, question: &str) -> Option<Target> {
        Some(Target::Question(rules::QuestionDraftKey {
            environment: self.snapshot.environment.clone(),
            thread: self.thread_id()?,
            request: request.into(),
            question: question.into(),
        }))
    }

    pub(super) fn answer_key_for(environment: &str, thread: &str, request: &str) -> String {
        serde_json::to_string(&(environment, thread, request)).expect("answer key")
    }

    pub(super) fn answer_key(&self, request: &str) -> String {
        Self::answer_key_for(
            &self.snapshot.environment,
            &self.thread_id().unwrap_or_default(),
            request,
        )
    }

    pub(super) fn question_responding(&self, request: &str, cx: &App) -> bool {
        !self.can_answer(cx)
            || !self
                .snapshot
                .pending
                .user_inputs
                .iter()
                .any(|input| input.request_id == request && input.can_respond())
            || self.responding.contains(&self.answer_key(request))
    }

    pub(super) fn can_answer(&self, cx: &App) -> bool {
        self.state.read(cx).client_handle().is_some_and(|client| {
            client
                .session_state()
                .borrow()
                .as_ref()
                .is_some_and(|grants| grants.grants(ORCHESTRATION_OPERATE_SCOPE))
        })
    }

    fn supports_question_attachments(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|config| {
                config.environment.capabilities.question_attachments == Some(true)
            })
    }

    pub(super) fn question_can_attach(
        &self,
        key: &rules::QuestionDraftKey,
        cx: &App,
        phase: rules::IntakePhase,
    ) -> bool {
        if key.environment != self.snapshot.environment {
            return false;
        }
        let state = self.state.read(cx);
        let Some(client) = state.client_handle() else {
            return false;
        };
        let Some(projection) = state.thread_state(&key.thread).projection.as_ref() else {
            return false;
        };
        let pending = t3_core::requests::pending_requests(projection);
        let Some((input, question)) = key.pending(&pending.user_inputs) else {
            return false;
        };
        rules::can_attach(
            question,
            input,
            self.supports_question_attachments(cx),
            client.session_state().borrow().as_ref(),
            self.responding.contains(&key.request_key()),
            phase,
        )
    }

    pub(super) fn reserved_attachment_count(&self, target: &Target) -> usize {
        match target {
            Target::Question(key) => self.attachments.staged.iter().filter(|(target, _)| matches!(target, Target::Question(other) if other.request_key() == key.request_key())).map(|(_, staged)| staged.len()).sum::<usize>()
                + self.attachments.preparations.keys().filter_map(|key| rules::QuestionDraftKey::parse(key)).filter(|other| other.request_key() == key.request_key()).map(|key| self.attachments.preparations.count(&key.storage_key())).sum::<usize>(),
            _ => self.staged_for(target).len(),
        }
    }

    fn question_attachments(&self, input: &PendingUserInput, question: &str) -> Vec<Attachment> {
        self.question_target(&input.request_id, question)
            .as_ref()
            .map(|target| {
                self.staged_for(target)
                    .iter()
                    .map(|staged| staged.attachment.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn question_answer_drafts(
        &self,
        input: &PendingUserInput,
        cx: &App,
    ) -> HashMap<String, DraftAnswer> {
        let stored = self.answers.get(&self.answer_key(&input.request_id));
        let all: Vec<Attachment> = input
            .questions
            .iter()
            .flat_map(|question| self.question_attachments(input, &question.id))
            .collect();
        let over_limit = attachments::limit_error(&all).is_some();
        input
            .questions
            .iter()
            .map(|question| {
                let attachments = self.question_attachments(input, &question.id);
                let preparing = self
                    .question_target(&input.request_id, &question.id)
                    .map_or(0, |target| {
                        self.attachments.preparations.count(&drafts::key(&target))
                    });
                let mut draft = rules::answer_draft(
                    stored.and_then(|answers| answers.drafts.get(&question.id)),
                    &attachments,
                    preparing,
                    self.supports_question_attachments(cx),
                );
                draft.attachments_blocked |=
                    over_limit || (!question.allows_custom_answer() && !attachments.is_empty());
                (question.id.clone(), draft)
            })
            .collect()
    }

    pub(super) fn question_response_attachments(
        &self,
        input: &PendingUserInput,
        cx: &App,
    ) -> Option<Map<String, Value>> {
        let drafts = self.question_answer_drafts(input, cx);
        t3_core::requests::build_answers(&input.questions, &drafts)?;
        let mut by_question = Map::new();
        for question in &input.questions {
            let attachments = self.question_attachments(input, &question.id);
            if attachments.is_empty() {
                continue;
            }
            let Target::Question(key) = self.question_target(&input.request_id, &question.id)?
            else {
                return None;
            };
            if !self.question_can_attach(&key, cx, rules::IntakePhase::Start) {
                return None;
            }
            by_question.insert(
                question.id.clone(),
                Value::Array(attachments::chat_attachments(&attachments)?),
            );
        }
        Some(by_question)
    }

    pub(super) fn prune_question_attachments(
        &mut self,
        environment: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut keys: Vec<String> = self
            .drafts
            .store
            .keys()
            .cloned()
            .chain(self.attachments.staged.keys().map(drafts::key))
            .chain(self.attachments.preparations.keys().cloned())
            .collect();
        keys.sort();
        keys.dedup();
        let questions: Vec<_> = keys
            .iter()
            .filter_map(|key| rules::QuestionDraftKey::parse(key))
            .collect();
        let mut pending_by_thread = HashMap::new();
        let state = self.state.read(cx);
        for key in &questions {
            if key.environment != environment || pending_by_thread.contains_key(&key.thread) {
                continue;
            }
            if let Some(projection) = state
                .thread_state(&key.thread)
                .projection
                .as_ref()
                .filter(|projection| projection.thread.id == key.thread)
            {
                pending_by_thread.insert(
                    key.thread.clone(),
                    projection
                        .runtime_requests
                        .iter()
                        .filter(|request| request.value["status"] == "pending")
                        .map(|request| request.id.clone())
                        .collect(),
                );
            }
        }
        let resolved = rules::resolved_draft_keys(&questions, environment, &pending_by_thread);
        let changed = !resolved.is_empty();
        for question in resolved {
            let key = question.storage_key();
            self.attachments.preparations.clear(&key);
            self.answers.remove(&question.request_key());
            self.responding.remove(&question.request_key());
            let target = Target::Question(question.clone());
            for staged in self.take_draft_attachments(&target) {
                if let Some(id) = staged.attachment.attachment_id() {
                    self.release_upload(id, cx);
                }
            }
            self.discard_draft(&target, cx);
        }
        changed
    }

    pub(super) fn watch_question_grants(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let connection = state.connection_id();
        if self.attachments.grant_watch.as_ref().map(|(id, _)| *id) == connection {
            return;
        }
        self.attachments.grant_watch = None;
        let (Some(id), Some(client)) = (connection, state.client_handle()) else {
            return;
        };
        let mut grants = client.session_state();
        self.attachments.grant_watch = Some((
            id,
            cx.spawn(async move |this, cx| {
                while grants.changed().await.is_ok() {
                    let _ = this.update(cx, |this, cx| {
                        this.cancel_pending_send();
                        this.finish_prepared_reads(cx);
                        cx.notify();
                    });
                }
            }),
        ));
    }

    pub(super) fn run_question_request(
        &mut self,
        key: String,
        command: Value,
        failure: &'static str,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "snapshot")]
        if let Some(spy) = cx.try_global::<QuestionCommandSpy>() {
            spy.count.set(spy.count.get() + 1);
            std::fs::write(
                &spy.output,
                serde_json::to_vec_pretty(
                    &serde_json::json!({"count":spy.count.get(),"command":command}),
                )
                .expect("command fixture"),
            )
            .expect("write command spy");
            cx.notify();
            return;
        }
        let request = self
            .state
            .read(cx)
            .rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                // Success stays guarded until the authoritative projection resolves the request.
                if let Err(error) = result {
                    this.responding.remove(&key);
                    if this
                        .active_input()
                        .is_some_and(|input| this.answer_key(&input.request_id) == key)
                    {
                        this.error = Some(format!("{failure} {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(feature = "snapshot")]
pub(crate) struct QuestionCommandSpy {
    pub output: std::path::PathBuf,
    pub count: std::cell::Cell<usize>,
}
#[cfg(feature = "snapshot")]
impl gpui::Global for QuestionCommandSpy {}

#[cfg(feature = "snapshot")]
impl Composer {
    pub(crate) fn seed_question_fixture(&mut self, value: &Value, cx: &mut Context<Self>) {
        use t3_core::composer::attachments::{AttachmentKind, Upload};
        let Some(input) = self.active_input().cloned() else {
            return;
        };
        for entry in value.as_array().into_iter().flatten() {
            let field = |name| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let Some(target) = self.question_target(&input.request_id, &field("question")) else {
                continue;
            };
            let upload = match field("status").as_str() {
                "uploading" => Upload::Uploading,
                "failed" => Upload::Failed {
                    reason: "Upload failed".into(),
                },
                _ => Upload::Ready {
                    attachment_id: format!("fixture-{}", field("id")),
                },
            };
            self.seed_attachments(
                &target,
                vec![Attachment {
                    id: field("id"),
                    kind: if field("type") == "image" {
                        AttachmentKind::Image
                    } else {
                        AttachmentKind::File
                    },
                    name: field("name"),
                    mime_type: field("mimeType"),
                    size_bytes: entry["sizeBytes"].as_u64().unwrap_or_default(),
                    pasted_text: false,
                    upload,
                }],
            );
            self.record_draft(&target, cx);
        }
        cx.notify();
    }
}
