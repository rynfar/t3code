//! What the agent waits on, attached above the composer: the question banner with its options
//! (`ComposerPendingUserInputPanel.tsx`), plus the answering flow from `usePendingThreadRequests.ts`:
//! the prompt edits the active question's own answer while the thread's draft waits aside.

use std::sync::Arc;

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::commands;
use t3_core::requests::{self, PendingUserInput, Question};

use super::controls::{Button, ButtonSize, ButtonVariant, checkbox, kbd, radio};
use super::{AUTO_ADVANCE, Composer, DisplacedAnswer, Target, drafts};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// `ComposerBanner` surface: the composer's surface (opaque, the no-blur fallback) inset 22px per
/// side and tucked 17px under the card, or detached (`rounded(14px)`) under the Draft Hero.
pub(crate) fn banner_surface_card(warning: bool, detached: bool, content: impl IntoElement, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let outline = if warning {
        hsla(p.warning).opacity(0.28)
    } else if dark {
        gpui::white().opacity(0.05)
    } else {
        hsla(p.foreground).opacity(0.08)
    };
    let card = div()
        .border_1()
        .border_color(outline)
        .bg(hsla(if dark { p.surface_raised } else { p.card }));
    if detached {
        return card.rounded(px(14.)).child(
            div()
                .rounded(px(13.))
                .py(px(6.))
                .px(px(6.))
                .when(warning, |this| this.bg(hsla(p.warning).opacity(0.08)))
                .text_xs()
                .line_height(px(16.))
                .child(content),
        );
    }
    card.mx(px(22.)).mb(px(-17.)).rounded_t(px(16.)).child(
        div()
            .rounded_t(px(15.))
            .pt(px(4.))
            .pb(px(21.))
            .px(px(4.))
            .when(warning, |this| this.bg(hsla(p.warning).opacity(0.08)))
            .text_xs()
            .line_height(px(16.))
            .child(content),
    )
}

pub(crate) fn banner_surface(warning: bool, content: impl IntoElement, cx: &App) -> gpui::Div {
    banner_surface_card(warning, false, content, cx)
}

impl Composer {
    pub(super) fn active_input(&self) -> Option<&PendingUserInput> {
        self.snapshot.pending.user_inputs.first()
    }

    fn question_index(&self, input: &PendingUserInput) -> usize {
        let index = self.answers.get(&self.answer_key(&input.request_id)).map(|answers| answers.index).unwrap_or(0);
        index.min(input.questions.len().saturating_sub(1))
    }

    pub(super) fn active_question(&self) -> Option<(String, Question)> {
        let input = self.active_input()?;
        Some((input.request_id.clone(), input.questions.get(self.question_index(input))?.clone()))
    }

    pub(super) fn progress(&self, cx: &gpui::App) -> Option<requests::Progress> {
        let input = self.active_input()?;
        let answers = self.answers.get(&self.answer_key(&input.request_id))?;
        Some(requests::progress(&input.questions, &self.question_answer_drafts(input, cx), answers.index))
    }

    /// Enters or leaves answering when the first pending question changes.
    pub(super) fn sync_answering(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_input().map(|input| self.answer_key(&input.request_id));
        if active == self.answering {
            return;
        }
        self.close_citation_comment("outside-press", window, cx);
        if self.answering.is_none() {
            self.stash = Some(self.prompt(cx));
        }
        self.answering = active.clone();
        self.question_collapsed = None;
        self.auto_advance = None;
        match active {
            Some(request_id) => {
                let questions = self.active_input().map(|input| input.questions.clone()).unwrap_or_default();
                let thread_id = self.thread_id().unwrap_or_default();
                let answers = self.answers.entry(request_id).or_insert_with(|| super::Answers { thread_id, ..Default::default() });
                requests::seed_drafts(&questions, &mut answers.drafts);
                self.show_active_answer(window, cx);
            }
            None => {
                let text = self.stash.take().unwrap_or_default();
                self.set_prompt(&text, window, cx);
            }
        }
    }

    /// Every question change lands here, so it also drops a pending auto-advance.
    fn show_active_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_pending_send();
        self.auto_advance = None;
        let text = self.progress(cx).map(|progress| progress.custom_answer).unwrap_or_default();
        self.set_prompt(&text, window, cx);
        if self.active_question().is_some_and(|(_, question)| !question.allows_custom_answer()) {
            window.focus(&self.question_focus, cx);
        }
    }

    /// `onAdvanceActivePendingUserInput`: next question, or submit on the last.
    pub(super) fn advance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(input) = self.active_input().cloned() else { return };
        if !self.can_answer(cx) || !input.can_respond() || self.question_responding(&input.request_id, cx) {
            return;
        }
        let drafts = self.question_answer_drafts(&input, cx);
        let key = self.answer_key(&input.request_id);
        let Some(answers) = self.answers.get_mut(&key) else { return };
        let progress = requests::progress(&input.questions, &drafts, answers.index);
        if progress.is_last_question {
            if let Some(mut built) = requests::build_answers(&input.questions, &drafts) {
                for (question_id, value) in built.iter_mut() {
                    let records = self.answer_context_records(&input.request_id, question_id);
                    *value = requests::project_pending_user_input_answer(std::mem::take(value), &records);
                }
                self.submit_answers(input.request_id, built, window, cx);
            }
            return;
        }
        if progress.can_advance {
            answers.index = progress.question_index + 1;
            self.show_active_answer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || !self.can_answer(cx) || self.active_input().is_some_and(|input| self.question_responding(&input.request_id, cx)) { return; }
        let Some(answers) = self.answering.as_ref().and_then(|request_id| self.answers.get_mut(request_id)) else { return };
        answers.index = answers.index.saturating_sub(1);
        self.show_active_answer(window, cx);
        cx.notify();
    }

    /// Picks an option. Single-select questions move on once the pick settles, as the web does.
    pub(super) fn toggle_option(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || !self.can_answer(cx) {
            return;
        }
        let Some((request_id, question)) = self.active_question() else { return };
        if self.question_responding(&request_id, cx) {
            return;
        }
        let key = self.answer_key(&request_id);
        let Some(answers) = self.answers.get_mut(&key) else { return };
        let draft = answers.drafts.get(&question.id);
        let displaced = draft.and_then(|draft| draft.custom.clone()).unwrap_or_default();
        let next = requests::toggle_option(&question, draft, &value);
        answers.drafts.insert(question.id.clone(), next);
        if let Some(stash) = self.stash.as_mut() {
            let next_stash = requests::carry_displaced_answer(stash, &displaced);
            let carried = !displaced.trim().is_empty() && next_stash != *stash;
            *stash = next_stash;
            if carried {
                if self.queue.editing.is_none()
                    && let Some(target) = self.snapshot.target.clone()
                {
                    self.displaced_answers.push(DisplacedAnswer {
                        draft: drafts::key(&target),
                        key: format!("{key}:{}", question.id),
                        text: displaced,
                    });
                    self.record_draft(&target, cx);
                }
                self.state.update(cx, |state, cx| {
                    state.show_toast(
                        crate::toast::Toast::new(crate::toast::ToastKind::Info, "Your typed text was set aside")
                            .description("It will be back in the composer once the question is answered."),
                        cx,
                    );
                });
            }
        }
        self.set_prompt("", window, cx);
        if !question.multi_select {
            let picked = (self.answer_key(&request_id), question.id);
            self.auto_advance = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.auto_advance = None;
                    let active = this.active_question().map(|(request_id, question)| (this.answer_key(&request_id), question.id));
                    if active.as_ref() == Some(&picked) {
                        this.advance(window, cx);
                    }
                });
            }));
        }
        cx.notify();
    }

    fn dispatch_question_answers(
        &mut self,
        thread_id: &str,
        request_id: &str,
        answers: serde_json::Map<String, serde_json::Value>,
        attachments: serde_json::Map<String, serde_json::Value>,
        retire_keys: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let key = self.answer_key(request_id);
        self.responding.insert(key.clone());
        let command = commands::respond_to_user_input_with_attachments(&uuid_v4(), thread_id, request_id, answers, attachments);
        self.run_question_request(key, command, retire_keys, "Failed to submit user input.", cx);
    }

    fn submit_answers(&mut self, request_id: String, answers: serde_json::Map<String, serde_json::Value>, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) || self.sending {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        let Some(input) = self.active_input().filter(|input| input.request_id == request_id && input.can_respond()).cloned() else { return };
        if !self.can_answer(cx) || self.question_responding(&request_id, cx) { return; }
        let question_targets: Vec<Target> = input
            .questions
            .iter()
            .filter_map(|q| self.question_target(&request_id, &q.id))
            .collect();
        for target in &question_targets {
            self.sync_upload_states_for(target, cx);
        }
        if question_targets.iter().any(|t| self.staged_for(t).iter().any(|s| s.needs_reattach)) {
            self.error = Some("Attach the interrupted file again or remove it".into());
            cx.notify();
            return;
        }
        let drafts = self.question_answer_drafts(&input, cx);
        if requests::build_answers(&input.questions, &drafts).is_none() {
            return;
        }
        match self.question_response_attachments(&input, cx) {
            Ok(attachments) => {
                let retire_keys = self.question_job_keys(&input);
                self.dispatch_question_answers(&thread_id, &request_id, answers, attachments, retire_keys, cx);
            }
            Err(requests::attachments::QuestionResponseAttachmentError::UploadsIncomplete { message, .. }) => {
                self.await_question_uploads(request_id, answers, message, window, cx);
            }
            Err(err) => {
                self.error = Some(err.message().to_owned());
                cx.notify();
            }
        }
    }

    fn await_question_uploads(
        &mut self,
        request_id: String,
        answers: serde_json::Map<String, serde_json::Value>,
        fallback_message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.active_input().filter(|input| input.request_id == request_id && input.can_respond()).cloned() else { return };
        let mut wait_tasks = Vec::new();
        for question in &input.questions {
            let Some(target) = self.question_target(&request_id, &question.id) else { continue };
            let staged: Vec<_> = self.staged_for(&target).iter().map(|s| s.attachment.clone()).collect();
            let retry_ids = t3_core::composer::attachments::retry_on_send(&staged);
            if let Some(task) = self.await_target_uploads(&target, &retry_ids, cx) {
                wait_tasks.push(task);
            }
        }
        if wait_tasks.is_empty() {
            self.error = Some(fallback_message);
            cx.notify();
            return;
        }
        let connection = self.state.read(cx).connection_id();
        let transport = self.state.read(cx).client_handle().map(|client| *client.connection_generation().borrow());
        self.send_connection = Some((connection, transport, self.state.read(cx).phase.clone()));
        self.pending_questions = self.question_revision(cx);
        let generation = self.pending_send.begin();
        self.sending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            futures::future::join_all(wait_tasks).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.pending_send.finish(generation) { return; }
                this.sending = false;
                if this.is_rewinding(cx) {
                    cx.notify();
                    return;
                }
                let Some(Target::Thread(thread_id)) = this.snapshot.target.clone() else {
                    cx.notify();
                    return;
                };
                let Some(input) = this.active_input().filter(|i| i.request_id == request_id && i.can_respond()).cloned() else {
                    cx.notify();
                    return;
                };
                for question in &input.questions {
                    if let Some(target) = this.question_target(&request_id, &question.id) {
                        this.sync_upload_states_for(&target, cx);
                    }
                }
                let same_connection = matches!(this.state.read(cx).phase, crate::app_state::Phase::Online(t3_client::supervisor::ConnectionStatus::Connected))
                    && this.question_revision(cx) == this.pending_questions
                    && this.state.read(cx).connection_id() == connection
                    && this.state.read(cx).client_handle().map(|client| *client.connection_generation().borrow()) == transport;
                if same_connection {
                    if !this.can_answer(cx) || this.question_responding(&request_id, cx) {
                        cx.notify();
                        return;
                    }
                    let drafts = this.question_answer_drafts(&input, cx);
                    if requests::build_answers(&input.questions, &drafts).is_none() {
                        cx.notify();
                        return;
                    }
                    match this.question_response_attachments(&input, cx) {
                        Ok(attachments) => {
                            let retire_keys = this.question_job_keys(&input);
                            this.dispatch_question_answers(&thread_id, &request_id, answers, attachments, retire_keys, cx);
                        }
                        Err(err) => {
                            this.error = Some(err.message().to_owned());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss(&mut self, request_id: String, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(Target::Thread(thread_id)) = self.snapshot.target.clone() else { return };
        if !self.can_answer(cx) || self.question_responding(&request_id, cx) || !self.active_input().is_some_and(|input| input.request_id == request_id && input.can_respond() && input.dismissible) { return; }
        let key = self.answer_key(&request_id);
        self.responding.insert(key.clone());
        let command = commands::dismiss_user_input(&uuid_v4(), &thread_id, &request_id);
        self.run_question_request(key, command, Vec::new(), "Failed to dismiss the question.", cx);
    }

    fn is_question_collapsed(&self, input: &PendingUserInput) -> bool {
        let index = self.question_index(input);
        input.questions.get(index).is_some_and(|question| self.question_collapsed.as_deref() == Some(question.id.as_str()))
    }

    /// The digit shortcuts (1–9) pick options while the prompt is not focused and the question is expanded.
    pub(super) fn pick_by_digit(&mut self, digit: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.is_rewinding(cx) || !self.can_answer(cx) {
            return false;
        }
        if self.active_input().is_some_and(|input| self.is_question_collapsed(input)) {
            return false;
        }
        let Some((_, question)) = self.active_question() else { return false };
        let Some(option) = question.options.get(digit.wrapping_sub(1)) else { return false };
        self.toggle_option(option.answer_value().to_owned(), window, cx);
        true
    }

    pub(super) fn answer_context_records(&self, request_id: &str, question_id: &str) -> Vec<serde_json::Value> {
        let mut records = Vec::new();
        if let Some(target) = self.snapshot.target.as_ref() {
            records.extend(self.mentions(target).iter().map(t3_core::composer::menu::ThreadContext::to_record));
            records.extend(self.reviews_for(target).iter().map(t3_core::composer::review::ReviewComment::record));
            records.extend(self.records_for(target).iter().map(|r| (**r).clone()));
        }
        if let Some(question_target) = self.question_target(request_id, question_id) {
            records.extend(self.mentions(&question_target).iter().map(t3_core::composer::menu::ThreadContext::to_record));
            records.extend(self.reviews_for(&question_target).iter().map(t3_core::composer::review::ReviewComment::record));
            records.extend(self.records_for(&question_target).iter().map(|r| (**r).clone()));
        }
        records
    }

    pub(super) fn withdraw_restored_answer(&mut self, request_id: &str, question_id: &str, value: &str, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let draft_key = drafts::key(&target);
        let answer_key = format!("{}:{question_id}", self.answer_key(request_id));
        let trimmed = value.trim();
        let Some(index) = self.displaced_answers.iter().rposition(|entry| {
            entry.draft == draft_key && entry.key == answer_key && entry.text.trim() == trimmed
        }) else {
            return;
        };
        let displaced = self.displaced_answers.remove(index);
        let later: Vec<String> = self.displaced_answers[index..]
            .iter()
            .filter(|entry| entry.draft == displaced.draft)
            .map(|entry| entry.text.clone())
            .collect();
        let later_refs: Vec<&str> = later.iter().map(String::as_str).collect();
        if self.queue.editing.is_none()
            && let Some(stash) = self.stash.as_mut()
        {
            let next = requests::withdraw_displaced_answer(stash, &displaced.text, &later_refs);
            if next != *stash {
                *stash = next;
                self.record_draft(&target, cx);
            }
        } else if let Some(saved) = self.drafts.store.get(&draft_key).cloned() {
            let next = requests::withdraw_displaced_answer(&saved.prompt, &displaced.text, &later_refs);
            if next != saved.prompt {
                let mut updated = saved;
                updated.prompt = next;
                if self.mutate_drafts(cx, |store| store.set(&draft_key, updated)) {
                    self.schedule_draft_save(&target, cx);
                }
            }
        }
    }

    /// Stop cancels an open question and a provider timeout expires it; neither sent the typed
    /// answers, so move unsent text + referenced thread chips back to the thread's own draft
    /// (never a queued-message edit) before pruning.
    pub(super) fn return_unanswered_user_inputs(
        &mut self,
        environment: &str,
        next_target: &Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut by_thread: std::collections::BTreeMap<String, Vec<(String, String)>> = std::collections::BTreeMap::new();
        for key in self.answers.keys() {
            if let Ok(parts) = serde_json::from_str::<Vec<String>>(key)
                && parts.len() == 3
                && parts[0] == environment
            {
                by_thread.entry(parts[1].clone()).or_default().push((parts[2].clone(), key.clone()));
            }
        }
        let mut changed = false;
        for (thread_id, entries) in by_thread {
            let unanswered = {
                let state = self.state.read(cx);
                let Some(projection) = state.thread_state(&thread_id).projection.as_ref().filter(|p| p.thread.id == thread_id) else {
                    continue;
                };
                requests::unanswered_request_ids(projection)
            };
            if unanswered.is_empty() {
                continue;
            }
            let target = Target::Thread(thread_id.clone());
            let draft_key = drafts::key(&target);
            let active_thread_draft = next_target.as_ref() == Some(&target) && self.queue.editing.is_none();
            let initial_prompt = if active_thread_draft {
                if self.answering.is_some() {
                    self.stash.clone().unwrap_or_else(|| self.drafts.store.get(&draft_key).map(|d| d.prompt.clone()).unwrap_or_default())
                } else {
                    self.prompt(cx)
                }
            } else {
                self.drafts.store.get(&draft_key).map(|d| d.prompt.clone()).unwrap_or_default()
            };
            let mut prompt = initial_prompt.clone();
            let mut restored_threads = Vec::new();
            let mut restored_records = Vec::new();
            for request_id in unanswered {
                let Some((_, answer_key)) = entries.iter().find(|(req, _)| req == &request_id) else { continue };
                let Some(answers) = self.answers.get(answer_key) else { continue };
                let mut draft_entries: Vec<_> = answers.drafts.iter().collect();
                draft_entries.sort_by_key(|&(a, _)| a);
                for (question_id, draft) in draft_entries {
                    let custom = draft.custom.as_deref().unwrap_or("");
                    let next_prompt = requests::carry_displaced_answer(&prompt, custom);
                    if next_prompt == prompt {
                        continue;
                    }
                    prompt = next_prompt;
                    let referenced = t3_core::composer::menu::referenced_context_ids(custom);
                    if referenced.is_empty() {
                        continue;
                    }
                    let q_target = Target::Question(t3_core::requests::attachments::QuestionDraftKey {
                        environment: environment.to_owned(),
                        thread: thread_id.clone(),
                        request: request_id.clone(),
                        question: question_id.clone(),
                    });
                    for src in [&q_target, &target] {
                        for thread in self.mentions(src) {
                            if referenced.contains(&thread.context_id.as_str()) {
                                restored_threads.push(thread.clone());
                            }
                        }
                        for rec in self.records_for(src) {
                            if t3_core::composer::records::field(rec, "kind") == "thread"
                                && referenced.contains(&t3_core::composer::records::field(rec, "contextId"))
                            {
                                restored_records.push(rec.clone());
                            }
                        }
                    }
                }
            }
            if prompt == initial_prompt {
                continue;
            }
            changed = true;
            self.restore_threads(&target, restored_threads);
            self.seed_records(&target, restored_records);
            if active_thread_draft {
                if self.answering.is_some() {
                    self.stash = Some(prompt.clone());
                } else {
                    self.set_prompt(&prompt, window, cx);
                }
            }
            let updated_draft = self.capture_draft(&target, prompt);
            if self.mutate_drafts(cx, |store| store.set(&draft_key, updated_draft)) {
                self.schedule_draft_save(&target, cx);
            }
        }
        changed
    }

    pub(super) fn render_question(&mut self, input: &PendingUserInput, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let index = self.question_index(input);
        let Some(question) = input.questions.get(index).cloned() else { return div().into_any_element() };
        let collapsed = self.question_collapsed.as_deref() == Some(question.id.as_str());
        let responding = self.is_rewinding(cx) || !self.can_answer(cx) || self.question_responding(&input.request_id, cx) || !input.can_respond();
        let drafts = self.answers.get(&self.answer_key(&input.request_id)).map(|answers| &answers.drafts);
        let draft = drafts.and_then(|drafts| drafts.get(&question.id)).cloned();
        let request_id = input.request_id.clone();
        let question_id = question.id.clone();
        let action_ctx = crate::transcript::markdown::links::MarkdownActionContext {
            state: self.state.clone(),
            owner: cx.entity().entity_id(),
            cwd: self.menu_cwd(cx),
            thread_id: self.thread_id(),
            thread_view: None,
        };
        let prompt_markdown = (!collapsed).then(|| {
            let text: Arc<str> = Arc::from(normalize_markdown_line_breaks(&question.question));
            let key = format!("composer-question:{}:{}", self.answer_key(&input.request_id), question.id);
            self.question_markdown.state_in_context(&key, &text, Some(action_ctx.clone()), cx)
        });
        let option_previews: Vec<Option<_>> = if collapsed {
            Vec::new()
        } else {
            let answer_key = self.answer_key(&input.request_id);
            question
                .options
                .iter()
                .enumerate()
                .map(|(position, option)| {
                    let preview = option.preview.as_deref().map(str::trim).filter(|p| !p.is_empty())?;
                    let text: Arc<str> = Arc::from(normalize_markdown_line_breaks(preview));
                    let key = format!("composer-question-preview:{answer_key}:{}:{position}", question.id);
                    Some(self.question_markdown.state_in_context(&key, &text, Some(action_ctx.clone()), cx))
                })
                .collect()
        };
        let collapsed_preview = question.question.replace('\n', " ");
        let content = v_flex()
            .child(
                h_flex()
                    .h(px(28.))
                    .pl(px(16.))
                    .pr(px(4.))
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .id("question-toggle")
                            .debug_selector(|| "question-toggle".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|this| this.bg(hsla(p.foreground).opacity(0.05)))
                            .tooltip(move |window, cx| {
                                let label = if collapsed { "Show the question and its options" } else { "Hide the question and its options" };
                                gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.question_collapsed = if collapsed { None } else { Some(question_id.clone()) };
                                cx.notify();
                            }))
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(hsla(p.muted_foreground))
                                            .when(collapsed, |this| this.max_w(gpui::relative(0.5)))
                                            .child(question.header.clone()),
                                    )
                                    .when(collapsed, |this| {
                                        this.child(div().flex_none().text_color(hsla(p.muted_foreground).opacity(0.4)).child("·"))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(hsla(p.secondary_label))
                                                    .child(collapsed_preview),
                                            )
                                    }),
                            )
                            .when(input.questions.len() > 1, |this| {
                                this.child(div().flex_none().px(px(6.)).text_color(hsla(p.muted_foreground)).child(format!("{}/{}", index + 1, input.questions.len())))
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(4.))
                                    .flex()
                                    .items_center()
                                    .child((if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }).render(px(14.), hsla(p.muted_foreground))),
                            ),
                    )
                    .when(input.dismissible, |this| {
                        this.child(
                            Button::new("question-dismiss", ButtonVariant::Outline, ButtonSize::IconXs)
                                .icon(Icon::X)
                                .tooltip("Dismiss question without answering")
                                .disabled(responding)
                                .on_click(cx.listener(move |this, _, _, cx| this.dismiss(request_id.clone(), cx))),
                        )
                    }),
            )
            .when_some(prompt_markdown, |this, state| {
                this.child(
                    v_flex()
                        .id("question-body")
                        .debug_selector(|| "question-body".to_owned())
                        .max_h(px(384.))
                        .overflow_y_scroll()
                        .pl(px(22.))
                        .pr(px(16.))
                        .pb(px(4.))
                        .child(div().pt(px(8.)).child(crate::transcript::markdown::prose(&state, cx)))
                        .child(v_flex().mt(px(6.)).ml(px(-6.)).children(question.options.iter().enumerate().map(|(position, option)| {
                            let value = option.answer_value().to_owned();
                            let selected = requests::is_option_selected(&question, draft.as_ref(), &value);
                            let preview = option_previews.get(position).and_then(|state| state.clone());
                            // The preview sits outside the option row: it can hold links, and clicking a single-select option advances.
                            v_flex()
                                .child(
                                    h_flex()
                                        .id(("question-option", position))
                                        .items_start()
                                        .gap(px(10.))
                                        .py(px(6.))
                                        .pl(px(6.))
                                        .rounded(px(6.))
                                        .text_color(if selected { hsla(p.foreground) } else { hsla(p.foreground).opacity(0.7) })
                                        .when(responding, |this| this.opacity(0.5))
                                        .when(!responding, |this| {
                                            this.cursor_pointer()
                                                .when(!selected, |this| this.hover(|this| this.bg(hsla(p.muted).opacity(0.3))))
                                                .on_click(cx.listener(move |this, _, window, cx| this.toggle_option(value.clone(), window, cx)))
                                        })
                                        .child(div().h(px(20.)).flex().items_center().child(if question.multi_select {
                                            checkbox(selected, cx)
                                        } else {
                                            radio(selected, cx)
                                        }))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .gap(px(2.))
                                                .child(div().text_sm().line_height(px(20.)).font_weight(FontWeight::MEDIUM).child(option.label.clone()))
                                                .when(!option.description.is_empty() && option.description != option.label, |this| {
                                                    this.child(div().text_size(px(11.)).text_color(hsla(p.secondary_label)).child(option.description.clone()))
                                                }),
                                        )
                                        .when(position < 9, |this| {
                                            this.child(div().h(px(20.)).w(px(24.)).flex().items_center().justify_center().child(kbd((position + 1).to_string(), cx)))
                                        }),
                                )
                                .when_some(preview, |this, state| {
                                    this.child(
                                        div()
                                            .id(("question-option-preview", position))
                                            .pl(px(32.))
                                            .pr(px(8.))
                                            .pb(px(6.))
                                            .text_xs()
                                            .text_color(hsla(p.muted_foreground))
                                            .child(crate::transcript::markdown::prose(&state, cx)),
                                    )
                                })
                        }))),
                )
            });
        banner_surface(false, content, cx)
            .track_focus(&self.question_focus)
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.question_focus, cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                if let Ok(digit) = keystroke.key.parse::<usize>()
                    && (1..=9).contains(&digit)
                    && this.pick_by_digit(digit, window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .into_any_element()
    }

    pub(super) fn render_reply_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let reply = self.current_reply_to()?;
        let citation = t3_core::composer::records::response_citation(reply)?;
        let title = match reply.get("subagents").and_then(serde_json::Value::as_array) {
            Some(subs) if !subs.is_empty() => {
                let names: Vec<&str> = subs.iter().filter_map(|s| s.get("title").and_then(serde_json::Value::as_str)).collect();
                if names.is_empty() { "Replying to".to_owned() } else { format!("Replying to {}", names.join(", ")) }
            }
            _ => "Replying to".to_owned(),
        };
        let label = citation.label();
        let state = self.state.clone();
        let p = palette(cx);
        let content = h_flex()
            .px(px(12.))
            .py(px(8.))
            .gap(px(8.))
            .items_center()
            .child(Icon::Reply.render(px(14.), hsla(p.muted_foreground)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .items_center()
                    .child(div().flex_none().text_xs().font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)).child(title))
                    .child(
                        div()
                            .id("composer-reply-banner-source")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(hsla(p.muted_foreground))
                            .hover(|this| this.text_color(hsla(p.foreground)))
                            .on_click(move |_, _, cx| {
                                crate::transcript::markdown::links::navigate_to_citation_source(citation.clone(), &state, cx);
                            })
                            .child(label),
                    ),
            )
            .child(
                Button::new("composer-dismiss-reply", ButtonVariant::Ghost, ButtonSize::IconXs)
                    .icon(Icon::X)
                    .tooltip("Cancel reply")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_reply_to(cx))),
            );
        Some(banner_surface(false, content, cx).into_any_element())
    }
}

/// Normalizes newlines and avoids Markdown `Node::Break` (`  \n` / `\\\n`), which
/// `gpui_component` drops; `markdown-rs` keeps raw single `\n` inside `Node::Text`.
fn normalize_markdown_line_breaks(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut fence: Option<(char, usize)> = None;
    let lines: Vec<&str> = normalized.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if let Some((fc, flen)) = fence {
            out.push_str(line);
            let trimmed = line.trim_start();
            let close_len = trimmed.bytes().take_while(|&b| b as char == fc).count();
            if close_len >= flen && trimmed[close_len..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        let leading = line.len() - trimmed.len();
        if leading <= 3
            && let Some(fc @ ('`' | '~')) = trimmed.chars().next()
        {
            let count = trimmed.bytes().take_while(|&b| b as char == fc).count();
            if count >= 3 {
                fence = Some((fc, count));
                out.push_str(line);
                continue;
            }
        }
        let has_next = index + 1 < lines.len();
        if has_next && !line.trim().is_empty() {
            let clean = line.trim_end_matches(' ');
            let clean = if clean.ends_with('\\') && !clean.ends_with("\\\\") {
                &clean[..clean.len() - 1]
            } else {
                clean
            };
            out.push_str(clean);
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize_markdown_line_breaks;

    #[test]
    fn normalize_markdown_line_breaks_strips_hard_break_markers_outside_fences() {
        let input = "line 1\r\nline 2  \nline 3\\\n```md\nkeep  \nkeep\\\n```\nline 4";
        assert_eq!(
            normalize_markdown_line_breaks(input),
            "line 1\nline 2\nline 3\n```md\nkeep  \nkeep\\\n```\nline 4"
        );
    }
}
