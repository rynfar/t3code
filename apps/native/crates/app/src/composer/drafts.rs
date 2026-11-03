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
    }
}

fn target(key: &str) -> Option<Target> {
    key.strip_prefix("thread:")
        .map(|id| Target::Thread(id.to_owned()))
        .or_else(|| key.strip_prefix("draft:").map(|project| Target::Draft(project.to_owned())))
}

impl Composer {
    /// Loads the saved drafts: choices, mentions and uploaded attachments go back to their
    /// targets; prompts load when a target opens.
    pub(super) fn load_drafts(&mut self, cx: &mut Context<Self>) {
        let store = DraftStore::from_value(store::load_ui_state(rules::STORE_NAME).as_ref());
        for key in store.keys() {
            let (Some(target), Some(draft)) = (target(key), store.get(key)) else { continue };
            self.choices.insert(
                target.clone(),
                Choices { model: draft.model_selection.clone(), runtime_mode: draft.runtime_mode, interaction_mode: draft.interaction_mode },
            );
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
        if let Some(target) = self.snapshot.target.clone() {
            self.record_draft(&target, cx);
        }
    }

    fn current_draft(&self, target: &Target, cx: &gpui::App) -> Draft {
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

    /// Forgets a new-thread draft once it launched, as the web deletes the draft thread.
    pub(super) fn discard_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.choices.remove(target);
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
        if let Target::Draft(project_id) = target { cx.emit(DraftsChanged { project_id: project_id.clone() }); }
        for key in self.drafts.store.take_pruned_keys() {
            if let Some(project_id) = key.strip_prefix("draft:") { cx.emit(DraftsChanged { project_id: project_id.to_owned() }); }
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

impl Composer {
    pub(crate) fn sidebar_drafts(&self) -> &DraftStore { &self.drafts.store }

    pub(crate) fn discard_sidebar_draft(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        let target = Target::Draft(project.into());
        self.record_draft(&target, cx);
        let draft = self.drafts.store.get(&key(&target))?.clone();
        self.discard_draft(&target, cx);
        let attachments = self.take_draft_attachments(&target);
        self.clear_mentions(&target);
        if self.snapshot.target.as_ref() == Some(&target) { self.set_prompt("", window, cx); }
        cx.notify();
        Some(DiscardedDraft { draft, attachments })
    }

    pub(crate) fn restore_sidebar_draft(&mut self, project: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        let target = Target::Draft(project.into());
        let DiscardedDraft { draft, attachments } = discarded;
        let live = self.current_draft(&target, cx);
        if !self.drafts.store.restore(&key(&target), draft.clone(), &live) {
            self.commit_sidebar_discard(DiscardedDraft { draft, attachments }, cx);
            self.error = Some("Failed to restore draft: The draft has new content.".into());
            cx.notify();
            return;
        }
        self.choices.insert(target.clone(), Choices { model: draft.model_selection.clone(), runtime_mode: draft.runtime_mode, interaction_mode: draft.interaction_mode });
        self.seed_mentions(&target, draft.thread_contexts());
        self.seed_terminals(&target, draft.terminals.clone());
        self.seed_reviews(&target, draft.review_comments.clone());
        self.restore_draft_attachments(&target, attachments, cx);
        if self.snapshot.target.as_ref() == Some(&target) { self.set_prompt(&draft.restored_prompt(), window, cx); }
        self.schedule_draft_save(&target, cx);
        cx.notify();
    }
}

impl Composer {
    pub(crate) fn commit_sidebar_discard(&mut self, discarded: DiscardedDraft, cx: &mut Context<Self>) {
        for attachment in discarded.attachments {
            if let Some(id) = attachment.attachment.attachment_id() { self.release_upload(id, cx); }
        }
    }
}
