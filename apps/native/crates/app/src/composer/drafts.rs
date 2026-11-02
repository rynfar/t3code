//! Per-thread drafts (`web/src/composerDraftStore.ts`): switching threads keeps each one's prompt,
//! choices, mentioned threads and attachments, and the drafts are written under
//! `T3_NATIVE_HOME` (atomically, debounced) so they survive a restart. The shape lives in
//! `t3_core::composer::drafts`.

use std::time::Duration;

use gpui::{Context, Task, Window};
use t3_client::store;
use t3_core::composer::drafts::{self as rules, Draft, DraftStore, SavedAttachment};

use super::{Choices, Composer, Target};
use crate::sidebar::controller::now_ms;

#[derive(Default)]
pub(super) struct Drafts {
    store: DraftStore,
    save: Option<Task<()>>,
}

fn key(target: &Target) -> String {
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
    pub(super) fn load_drafts(&mut self) {
        let store = DraftStore::from_value(store::load_ui_state(rules::STORE_NAME).as_ref());
        for key in store.keys() {
            let (Some(target), Some(draft)) = (target(key), store.get(key)) else { continue };
            self.choices.insert(
                target.clone(),
                Choices { model: draft.model_selection.clone(), runtime_mode: draft.runtime_mode, interaction_mode: draft.interaction_mode },
            );
            self.seed_mentions(&target, draft.thread_contexts());
            self.seed_attachments(&target, draft.attachments.iter().map(SavedAttachment::restore).collect());
        }
        self.drafts.store = store;
    }

    fn capture_draft(&self, target: &Target, prompt: String) -> Draft {
        let choices = self.choices.get(target).cloned().unwrap_or_default();
        Draft {
            threads: Draft::saved_threads(self.mentions(target), &prompt),
            attachments: self.staged_for(target).iter().filter_map(|staged| SavedAttachment::of(&staged.attachment)).collect(),
            prompt,
            model_selection: choices.model,
            runtime_mode: choices.runtime_mode,
            interaction_mode: choices.interaction_mode,
            updated_at: now_ms(),
        }
    }

    /// The prompt a target's draft holds: the text, unless the prompt shows a question's answer.
    fn draft_prompt(&self, cx: &gpui::App) -> String {
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

    /// Records `target`'s draft; one not open keeps its saved prompt.
    pub(super) fn record_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        let prompt = match self.snapshot.target.as_ref() == Some(target) {
            true => self.draft_prompt(cx),
            false => self.drafts.store.get(&key(target)).map(|draft| draft.prompt.clone()).unwrap_or_default(),
        };
        let draft = self.capture_draft(target, prompt);
        if self.drafts.store.set(&key(target), draft) {
            self.schedule_draft_save(cx);
        }
    }

    /// Forgets a new-thread draft once it launched, as the web deletes the draft thread.
    pub(super) fn discard_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.choices.remove(target);
        if self.drafts.store.remove(&key(target)) {
            self.schedule_draft_save(cx);
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
            let draft = self.capture_draft(from, leaving);
            if self.drafts.store.set(&key(from), draft) {
                self.schedule_draft_save(cx);
            }
        }
        let incoming = to.and_then(|to| self.drafts.store.get(&key(to))).map(|draft| draft.prompt.clone()).unwrap_or_default();
        self.set_prompt(&incoming, window, cx);
        let end = incoming.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
    }

    fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        self.drafts.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PERSIST_DEBOUNCE_MS)).await;
            let _ = this.update(cx, |this, _| store::save_ui_state(rules::STORE_NAME, &this.drafts.store.to_value()));
        }));
    }
}
