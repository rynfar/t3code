//! Per-thread drafts (`web/src/composerDraftStore.ts`): switching threads keeps each one's prompt,
//! choices, mentioned threads, terminal excerpts and attachments, and the drafts are written under
//! `T3_NATIVE_HOME` (atomically, debounced) so they survive a restart. The shape lives in
//! `t3_core::composer::drafts`.

use std::time::Duration;

use gpui::{Context, EventEmitter, Task, Window};
use t3_client::store;
use t3_core::composer::drafts::{self as rules, Draft, DraftStore, SavedAttachment};

use super::{Choices, Composer, Target};
use crate::sidebar::controller::now_ms;

#[derive(Default)]
pub(super) struct Drafts {
    pub(super) store: DraftStore,
    save: Option<Task<()>>,
}

pub(super) fn key(target: &Target) -> String {
    match target {
        Target::Thread(id) => rules::thread_key(id),
        Target::Draft(project) => rules::new_thread_key(project),
        Target::Question(key) => key.storage_key(),
    }
}

pub(super) fn target(key: &str) -> Option<Target> {
    key.strip_prefix("thread:")
        .map(|id| Target::Thread(id.to_owned()))
        .or_else(|| key.strip_prefix("draft:").map(|project| Target::Draft(project.to_owned())))
        .or_else(|| t3_core::requests::attachments::QuestionDraftKey::parse(key).map(Target::Question))
}

impl Composer {
    pub(super) fn restore_draft_choices(&mut self, target: &Target, draft: &Draft, cx: &mut Context<Self>) {
        self.choices.insert(target.clone(), Choices { model: draft.model_selection.clone(), runtime_mode: draft.runtime_mode, interaction_mode: draft.interaction_mode });
        if self.snapshot.target.as_ref() == Some(target) { self.sync_provider_target(cx); }
    }

    /// Loads the saved drafts: choices, mentions and uploaded attachments go back to their
    /// targets; prompts load when a target opens.
    pub(super) fn load_drafts(&mut self, cx: &mut Context<Self>) {
        let store = DraftStore::from_value(store::load_ui_state(rules::STORE_NAME).as_ref());
        for key in store.keys() {
            let (Some(target), Some(draft)) = (target(key), store.get(key)) else { continue };
            self.restore_draft_choices(&target, draft, cx);
            self.seed_records(&target, draft.context_records.clone());
            self.seed_mentions(&target, draft.thread_contexts());
            self.seed_terminals(&target, draft.terminals.clone());
        self.seed_reviews(&target, draft.review_comments.clone());
            self.publish_review_comments(&target, &draft.prompt, cx);
            self.seed_attachments(&target, draft.attachments.iter().map(SavedAttachment::restore).collect());
        }
        self.drafts.store = store;
    }

    fn publish_review_comments(&self, target: &Target, prompt: &str, cx: &mut Context<Self>) {
        if let Target::Thread(thread) = target {
            let comments = t3_core::composer::review::referenced(prompt, self.reviews_for(target)).into_iter().cloned().collect();
            self.state.update(cx, |state, cx| state.set_review_comments(thread, comments, cx));
        }
    }

    fn capture_draft(&self, target: &Target, prompt: String) -> Draft {
        let choices = self.choices.get(target).cloned().unwrap_or_default();
        Draft {
            workspace: self.drafts.store.get(&key(target)).and_then(|draft| draft.workspace.clone()),
            context_records: t3_core::composer::records::referenced_shared(&prompt, self.records_for(target)),
            threads: Draft::saved_threads(self.mentions(target), &prompt),
            terminals: Draft::saved_terminals(self.terminals_for(target), &prompt),
            review_comments: t3_core::composer::review::referenced(&prompt, self.reviews_for(target)).into_iter().cloned().collect(),
            attachments: self.staged_for(target).iter().filter_map(|staged| SavedAttachment::of(&staged.attachment)).collect(),
            prompt,
            model_selection: choices.model,
            runtime_mode: choices.runtime_mode,
            interaction_mode: choices.interaction_mode,
            pending_attachment_count: self.staged_for(target).iter().filter(|staged| staged.attachment.attachment_id().is_none()).count(),
            created_at: None,
            updated_at: now_ms(),
        }
    }

    /// The prompt a target's draft holds: the text, unless the prompt shows a question's answer.
    pub(super) fn draft_prompt(&self, cx: &gpui::App) -> String {
        if let Some(editing) = self.editing_queued() {
            return editing.stash.clone();
        }
        match self.answering {
            Some(_) => self.stash.clone().unwrap_or_default(),
            None => self.prompt(cx),
        }
    }

    /// Records the current target's draft and schedules a write.
    pub(super) fn draft_changed(&mut self, cx: &mut Context<Self>) {
        let choice = self.choices().and_then(|choices| choices.model.as_ref()).map(|model| &model.instance_id);
        if choice != self.provider_choice.as_ref() { self.sync_provider_target(cx); }
        if self.cancel_pending_send() { cx.notify(); }
        if let Some(target) = self.snapshot.target.clone() {
            self.record_draft(&target, cx);
        }
    }

    pub(super) fn current_draft(&self, target: &Target, cx: &gpui::App) -> Draft {
        let prompt = match self.snapshot.target.as_ref() == Some(target) {
            true => self.draft_prompt(cx),
            false => self.drafts.store.get(&key(target)).map(|draft| draft.prompt.clone()).unwrap_or_default(),
        };
        self.capture_draft(target, prompt)
    }

    /// Records `target`'s draft; one not open keeps its saved prompt.
    pub(super) fn record_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        let draft = self.current_draft(target, cx);
        self.publish_review_comments(target, &draft.prompt, cx);
        if self.drafts.store.set(&key(target), draft) {
            self.schedule_draft_save(target, cx);
        }
    }

    /// Merges a completed rewind's prompt + attachments into `thread_id`'s stored draft while another thread is open.
    pub(super) fn merge_off_thread_restore(
        &mut self,
        thread_id: &str,
        restored: &str,
        attachments: Vec<crate::app_state::RestoredAttachment>,
        cx: &mut Context<Self>,
    ) {
        let target = Target::Thread(thread_id.to_owned());
        if !restored.is_empty() {
            let current = self.drafts.store.get(&key(&target)).map(|draft| draft.prompt.as_str()).unwrap_or_default();
            let merged = t3_core::timeline::merge_restored_prompt(current, restored);
            let draft = self.capture_draft(&target, merged);
            if self.drafts.store.set(&key(&target), draft) {
                self.schedule_draft_save(&target, cx);
            }
        }
        self.restore_reverted_attachments(thread_id, attachments, cx);
    }

    /// Forgets a new-thread draft once it launched, as the web deletes the draft thread.
    pub(super) fn discard_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.choices.remove(target);
        self.sync_provider_target(cx);
        if self.drafts.store.remove(&key(target)) {
            self.schedule_draft_save(target, cx);
        }
    }

    /// Saves the target being left and shows the one being opened.
    pub(super) fn switch_draft(&mut self, from: Option<&Target>, to: Option<&Target>, window: &mut Window, cx: &mut Context<Self>) {
        // An open question's answer and a queued edit are not the draft; what they set aside is.
        let answering = self.answering.take().is_some();
        let stash = self.stash.take();
        let leaving = match (self.queue.editing.take(), answering) {
            (Some(editing), _) => editing.stash,
            (None, true) => stash.unwrap_or_default(),
            (None, false) => self.prompt(cx),
        };
        if let Some(from) = from {
            self.reset_context_history(from, &leaving);
            self.publish_review_comments(from, &leaving, cx);
            let draft = self.capture_draft(from, leaving);
            if self.drafts.store.set(&key(from), draft) {
                self.schedule_draft_save(from, cx);
            }
        }
        let incoming = to.and_then(|to| self.drafts.store.get(&key(to))).map(Draft::restored_prompt).unwrap_or_default();
        self.set_prompt(&incoming, window, cx);
        let end = incoming.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
    }

    pub(super) fn schedule_draft_save(&mut self, target: &Target, cx: &mut Context<Self>) {
        match target {
            Target::Draft(project_id) => cx.emit(DraftsChanged { project_id: project_id.clone() }),
            Target::Thread(_) => cx.emit(ThreadDraftsChanged),
            Target::Question(_) => {}
        }
        for key in self.drafts.store.take_pruned_keys() {
            if let Some(project_id) = key.strip_prefix("draft:") { cx.emit(DraftsChanged { project_id: project_id.to_owned() }); }
            if key.starts_with("thread:") { cx.emit(ThreadDraftsChanged); }
        }
        self.drafts.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PERSIST_DEBOUNCE_MS)).await;
            let _ = this.update(cx, |this, _| store::save_ui_state(rules::STORE_NAME, &this.drafts.store.to_value()));
        }));
    }
}

pub(crate) struct DiscardedDraft {
    draft: Draft,
    attachments: Vec<super::attachments::Staged>,
}

pub(crate) struct DraftsChanged { pub project_id: String }
impl EventEmitter<DraftsChanged> for Composer {}

pub(crate) struct ThreadDraftsChanged;
impl EventEmitter<ThreadDraftsChanged> for Composer {}

impl Composer {
    pub(crate) fn sidebar_drafts(&self) -> &DraftStore { &self.drafts.store }

    pub(crate) fn discard_sidebar_draft(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        self.discard_target_draft(&Target::Draft(project.into()), false, window, cx)
    }

    pub(crate) fn discard_thread_draft(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        self.discard_target_draft(&Target::Thread(thread_id.into()), true, window, cx)
    }

    fn discard_target_draft(&mut self, target: &Target, keep_choices: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        self.record_draft(target, cx);
        let storage_key = key(target);
        let draft = self.drafts.store.get(&storage_key)?.clone();
        if keep_choices && !draft.has_user_content() { return None; }
        if keep_choices {
            if self.drafts.store.clear_content(&storage_key, now_ms()) {
                self.schedule_draft_save(target, cx);
            }
        } else {
            self.discard_draft(target, cx);
        }
        let attachments = self.take_draft_attachments(target);
        self.clear_mentions(target);
        if self.snapshot.target.as_ref() == Some(target) { self.set_prompt("", window, cx); }
        cx.notify();
        Some(DiscardedDraft { draft, attachments })
    }

    pub(crate) fn restore_sidebar_draft(&mut self, project: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&Target::Draft(project.into()), discarded, window, cx);
    }

    pub(crate) fn restore_thread_draft(&mut self, thread_id: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&Target::Thread(thread_id.into()), discarded, window, cx);
    }

    fn restore_target_draft(&mut self, target: &Target, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        let DiscardedDraft { draft, attachments } = discarded;
        let live = self.current_draft(target, cx);
        if !self.drafts.store.restore(&key(target), draft.clone(), &live) {
            self.commit_sidebar_discard(DiscardedDraft { draft, attachments }, cx);
            self.error = Some("Failed to restore draft: The draft has new content.".into());
            cx.notify();
            return;
        }
        self.restore_draft_choices(target, &draft, cx);
        self.seed_records(target, draft.context_records.clone());
        self.seed_mentions(target, draft.thread_contexts());
        self.seed_terminals(target, draft.terminals.clone());
        self.seed_reviews(target, draft.review_comments.clone());
        self.restore_draft_attachments(target, attachments, cx);
        if self.snapshot.target.as_ref() == Some(target) { self.set_prompt(&draft.restored_prompt(), window, cx); }
        self.schedule_draft_save(target, cx);
        cx.notify();
    }

    /// Moves a non-empty draft from `from_project` to `to_project` when the user switches projects
    /// in the draft hero (`DraftHeroHeadline.tsx`: `selectProject`). Empty drafts are left alone so
    /// switching away never clobbers the destination project's saved draft.
    pub(crate) fn retarget_draft(
        &mut self,
        from_project: &str,
        to_project: &str,
        keep_attachments: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if from_project == to_project {
            return false;
        }
        let from = Target::Draft(from_project.to_owned());
        let to = Target::Draft(to_project.to_owned());
        let mut draft = self.current_draft(&from, cx);
        if !draft.has_user_content() {
            return false;
        }
        if !keep_attachments
            && draft.prompt.trim().is_empty()
            && draft.terminals.is_empty()
            && draft.review_comments.is_empty()
        {
            return false;
        }
        draft.workspace = None;
        let attachments = if keep_attachments {
            self.take_draft_attachments(&from)
        } else {
            let dropped = self.take_draft_attachments(&from);
            self.commit_sidebar_discard(DiscardedDraft { draft: Draft::default(), attachments: dropped }, cx);
            draft.attachments.clear();
            draft.pending_attachment_count = 0;
            Vec::new()
        };
        let old_to = self.take_draft_attachments(&to);
        self.commit_sidebar_discard(DiscardedDraft { draft: Draft::default(), attachments: old_to }, cx);
        self.discard_draft(&from, cx);
        self.clear_mentions(&from);
        self.context.records.remove(&from);
        self.context.reviews.remove(&from);
        self.clear_terminals(&from);
        if self.snapshot.target.as_ref() == Some(&from) {
            self.set_prompt("", window, cx);
        }
        self.context.records.remove(&to);
        self.context.reviews.remove(&to);
        self.clear_terminals(&to);
        self.clear_mentions(&to);
        self.restore_draft_choices(&to, &draft, cx);
        self.seed_records(&to, draft.context_records.clone());
        self.seed_mentions(&to, draft.thread_contexts());
        self.seed_terminals(&to, draft.terminals.clone());
        self.seed_reviews(&to, draft.review_comments.clone());
        if self.drafts.store.set(&key(&to), draft.clone()) {
            self.schedule_draft_save(&to, cx);
        }
        self.restore_draft_attachments(&to, attachments, cx);
        if self.snapshot.target.as_ref() == Some(&to) {
            self.set_prompt(&draft.restored_prompt(), window, cx);
        }
        cx.notify();
        true
    }
}

impl Composer {
    pub(crate) fn commit_sidebar_discard(&mut self, discarded: DiscardedDraft, cx: &mut Context<Self>) {
        for attachment in discarded.attachments {
            if let Some(id) = attachment.attachment.attachment_id() { self.release_upload(id, cx); }
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn set_thread_draft_fixture(
        &mut self,
        thread_id: &str,
        prompt: &str,
        model_selection: Option<t3_core::model::ModelSelection>,
        updated_at: i64,
        cx: &mut Context<Self>,
    ) {
        let target = Target::Thread(thread_id.to_owned());
        let draft = Draft { prompt: prompt.to_owned(), model_selection, updated_at, ..Default::default() };
        self.restore_draft_choices(&target, &draft, cx);
        let key = key(&target);
        if self.drafts.store.set(&key, draft) {
            cx.emit(ThreadDraftsChanged);
        }
        cx.notify();
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn set_test_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_prompt(text, window, cx);
        self.draft_changed(cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn test_prompt(&self, cx: &gpui::App) -> String {
        self.prompt(cx)
    }
}
