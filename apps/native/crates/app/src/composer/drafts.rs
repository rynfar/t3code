//! Per-thread drafts (`web/src/composerDraftStore.ts`): switching threads keeps each one's prompt,
//! choices, mentioned threads, terminal excerpts and attachments, and the drafts are written under
//! `T3_NATIVE_HOME` (atomically, debounced) so they survive a restart. The shape lives in
//! `t3_core::composer::drafts`.

use std::time::Duration;

use gpui::{BorrowAppContext, Context, EventEmitter, Task, Window};
use t3_client::store;
use t3_core::composer::drafts::{self as rules, Draft, DraftStore, SavedAttachment};

use super::{Choices, Composer, Target};
use crate::sidebar::controller::now_ms;

#[derive(Default)]
pub(super) struct Drafts {
    pub(super) store: DraftStore,
    pruned: Vec<String>,
}

#[derive(Default)]
struct SharedDraftStore {
    loaded: bool,
    store: DraftStore,
    save: Option<Task<()>>,
}

impl gpui::Global for SharedDraftStore {}

fn load_shared_store(cx: &mut gpui::App) -> DraftStore {
    let loaded = cx.default_global::<SharedDraftStore>().loaded;
    if !loaded {
        let initial = DraftStore::from_value(store::load_ui_state(rules::STORE_NAME).as_ref());
        cx.update_global::<SharedDraftStore, _>(|shared, _| {
            shared.store = initial;
            shared.loaded = true;
        });
    }
    cx.global::<SharedDraftStore>().store.clone()
}

fn live_composers(cx: &gpui::App) -> Vec<gpui::Entity<Composer>> {
    cx.try_global::<super::RegisteredComposers>()
        .map(|reg| reg.0.iter().filter_map(|weak| weak.upgrade()).collect())
        .unwrap_or_default()
}

fn sync_composers_draft_store(skip: Option<gpui::EntityId>, snapshot: &DraftStore, cx: &mut gpui::App) {
    for composer in live_composers(cx) {
        if Some(composer.entity_id()) == skip {
            continue;
        }
        let next = snapshot.clone();
        composer.update(cx, |composer, cx| {
            composer.drafts.store = next;
            cx.notify();
        });
    }
}

fn save_shared_now(cx: &mut gpui::App) {
    let value = cx.update_global::<SharedDraftStore, _>(|shared, _| {
        shared.save = None;
        shared.store.to_value()
    });
    store::save_ui_state(rules::STORE_NAME, &value);
}

fn emit_draft_events(target: &Target, pruned: &[String], cx: &mut Context<Composer>) {
    match target {
        Target::Draft(project_id) => cx.emit(DraftsChanged { project_id: project_id.clone() }),
        Target::Thread(_) => cx.emit(ThreadDraftsChanged),
        Target::Question(_) => {}
    }
    for key in pruned {
        if let Some(project_id) = key.strip_prefix("draft:") { cx.emit(DraftsChanged { project_id: project_id.to_owned() }); }
        if key.starts_with("thread:") { cx.emit(ThreadDraftsChanged); }
    }
}

fn broadcast_draft_events(target: &Target, pruned: &[String], self_id: gpui::EntityId, cx: &mut Context<Composer>) {
    emit_draft_events(target, pruned, cx);
    for composer in live_composers(cx) {
        if composer.entity_id() == self_id {
            continue;
        }
        let t = target.clone();
        let p = pruned.to_vec();
        composer.update(cx, |_, cx| emit_draft_events(&t, &p, cx));
    }
}

pub(super) fn key(target: &Target) -> String {
    match target {
        Target::Thread(id) => rules::thread_key(id),
        Target::Draft(project) => rules::new_thread_key(project),
        Target::Question(key) => key.storage_key(),
    }
}

/// Reads the persisted draft prompt for `storage_key` (`backgroundThread.ts` for `pingdotgg/t3code#35761`).
pub(super) fn read_stored_prompt(storage_key: &str, cx: &gpui::App) -> String {
    if let Some(shared) = cx.try_global::<SharedDraftStore>()
        && shared.loaded
    {
        return shared
            .store
            .get(storage_key)
            .map(Draft::restored_prompt)
            .unwrap_or_default();
    }
    DraftStore::from_value(store::load_ui_state(rules::STORE_NAME).as_ref())
        .get(storage_key)
        .map(Draft::restored_prompt)
        .unwrap_or_default()
}

/// Commits a dictated prompt directly into a background target's stored draft (`pingdotgg/t3code#35761`).
pub(super) fn write_stored_prompt(storage_key: &str, prompt: String, cx: &mut gpui::App) {
    let _ = load_shared_store(cx);
    let (changed, snapshot) = cx.update_global::<SharedDraftStore, _>(|shared, _| {
        let mut draft = shared.store.get(storage_key).cloned().unwrap_or_default();
        draft.prompt = prompt;
        draft.updated_at = now_ms();
        let changed = shared.store.set(storage_key, draft);
        (changed, shared.store.clone())
    });
    if changed {
        sync_composers_draft_store(None, &snapshot, cx);
        save_shared_now(cx);
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
        self.choices.insert(target.clone(), Choices { model: draft.model_selection.clone(), multi_models: Vec::new(), runtime_mode: draft.runtime_mode, interaction_mode: draft.interaction_mode });
        if self.snapshot.target.as_ref() == Some(target) { self.sync_provider_target(cx); }
    }

    /// Loads the saved drafts: choices, mentions and uploaded attachments go back to their
    /// targets; prompts load when a target opens.
    pub(super) fn load_drafts(&mut self, cx: &mut Context<Self>) {
        let store = load_shared_store(cx);
        for key in store.keys() {
            let (Some(target), Some(draft)) = (target(key), store.get(key)) else { continue };
            self.restore_draft_choices(&target, draft, cx);
            self.seed_records(&target, draft.context_records.clone());
            self.seed_mentions(&target, draft.thread_contexts());
            self.seed_terminals(&target, draft.terminals.clone());
        self.seed_reviews(&target, draft.review_comments.clone());
            self.seed_reply_to(&target, draft.reply_to.clone());
            self.publish_review_comments(&target, &draft.prompt, cx);
            self.seed_attachments(&target, draft.attachments.iter().map(SavedAttachment::restore).collect(), true, cx);
        }
        self.drafts.store = store;
    }

    pub(super) fn mutate_drafts<R>(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut DraftStore) -> R) -> R {
        let _ = load_shared_store(cx);
        let (out, pruned, snapshot) = cx.update_global::<SharedDraftStore, _>(|shared, _| {
            let out = edit(&mut shared.store);
            let pruned = shared.store.take_pruned_keys();
            (out, pruned, shared.store.clone())
        });
        self.drafts.store = snapshot.clone();
        self.drafts.pruned.extend(pruned);
        sync_composers_draft_store(Some(cx.entity_id()), &snapshot, cx);
        out
    }

    fn publish_review_comments(&self, target: &Target, prompt: &str, cx: &mut Context<Self>) {
        if let Target::Thread(thread) = target {
            let comments = t3_core::composer::review::referenced(prompt, self.reviews_for(target)).into_iter().cloned().collect();
            self.state.update(cx, |state, cx| state.set_review_comments(thread, comments, cx));
        }
    }

    pub(super) fn capture_draft(&self, target: &Target, prompt: String) -> Draft {
        let choices = self.choices.get(target).cloned().unwrap_or_default();
        Draft {
            workspace: self.drafts.store.get(&key(target)).and_then(|draft| draft.workspace.clone()),
            context_records: t3_core::composer::records::referenced_shared(&prompt, self.records_for(target)),
            threads: Draft::saved_threads(self.mentions(target), &prompt),
            terminals: Draft::saved_terminals(self.terminals_for(target), &prompt),
            review_comments: t3_core::composer::review::referenced(&prompt, self.reviews_for(target)).into_iter().cloned().collect(),
            reply_to: self.reply_to_for(target).cloned(),
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
        self.sync_branch_mismatch(cx);
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
        if self.mutate_drafts(cx, |store| store.set(&key(target), draft)) {
            self.schedule_draft_save(target, cx);
        }
    }

    /// Forgets a new-thread draft once it launched, as the web deletes the draft thread.
    pub(super) fn discard_draft(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.choices.remove(target);
        self.sync_provider_target(cx);
        if self.mutate_drafts(cx, |store| store.remove(&key(target))) {
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
            if self.mutate_drafts(cx, |store| store.set(&key(from), draft)) {
                self.schedule_draft_save(from, cx);
            }
        }
        let incoming = to.and_then(|to| self.drafts.store.get(&key(to))).map(Draft::restored_prompt).unwrap_or_default();
        self.set_prompt(&incoming, window, cx);
        let end = incoming.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
    }

    pub(super) fn schedule_draft_save(&mut self, target: &Target, cx: &mut Context<Self>) {
        let pruned = std::mem::take(&mut self.drafts.pruned);
        broadcast_draft_events(target, &pruned, cx.entity_id(), cx);
        let task = gpui::App::spawn(cx, async move |cx| {
            cx.background_executor().timer(Duration::from_millis(rules::PERSIST_DEBOUNCE_MS)).await;
            cx.update(save_shared_now);
        });
        cx.update_global::<SharedDraftStore, _>(|shared, _| {
            shared.save = Some(task);
        });
    }
}

pub(crate) struct DiscardedDraft {
    target: Target,
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
            if self.mutate_drafts(cx, |store| store.clear_content(&storage_key, now_ms())) {
                self.schedule_draft_save(target, cx);
            }
        } else {
            self.discard_draft(target, cx);
        }
        let attachments = self.take_draft_attachments(target);
        self.clear_mentions(target);
        self.take_reply_to(target);
        if self.snapshot.target.as_ref() == Some(target) { self.set_prompt("", window, cx); }
        cx.notify();
        Some(DiscardedDraft { target: target.clone(), draft, attachments })
    }

    pub(crate) fn restore_sidebar_draft(&mut self, project: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&Target::Draft(project.into()), discarded, window, cx);
    }

    pub(crate) fn restore_thread_draft(&mut self, thread_id: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&Target::Thread(thread_id.into()), discarded, window, cx);
    }

    fn restore_target_draft(&mut self, target: &Target, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        let DiscardedDraft { target: discarded_target, draft, attachments } = discarded;
        let live = self.current_draft(target, cx);
        if !self.mutate_drafts(cx, |store| store.restore(&key(target), draft.clone(), &live)) {
            self.commit_sidebar_discard(DiscardedDraft { target: discarded_target, draft, attachments }, cx);
            self.error = Some("Failed to restore draft: The draft has new content.".into());
            cx.notify();
            return;
        }
        self.restore_draft_choices(target, &draft, cx);
        self.seed_records(target, draft.context_records.clone());
        self.seed_mentions(target, draft.thread_contexts());
        self.seed_terminals(target, draft.terminals.clone());
        self.seed_reviews(target, draft.review_comments.clone());
        self.seed_reply_to(target, draft.reply_to.clone());
        self.restore_draft_attachments(target, None, attachments, cx);
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
            self.commit_sidebar_discard(DiscardedDraft { target: from.clone(), draft: Draft::default(), attachments: dropped }, cx);
            draft.attachments.clear();
            draft.pending_attachment_count = 0;
            Vec::new()
        };
        let old_to = self.take_draft_attachments(&to);
        self.commit_sidebar_discard(DiscardedDraft { target: to.clone(), draft: Draft::default(), attachments: old_to }, cx);
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
        if self.mutate_drafts(cx, |store| store.set(&key(&to), draft.clone())) {
            self.schedule_draft_save(&to, cx);
        }
        self.restore_draft_attachments(&to, keep_attachments.then_some(&from), attachments, cx);
        if self.snapshot.target.as_ref() == Some(&to) {
            self.set_prompt(&draft.restored_prompt(), window, cx);
        }
        cx.notify();
        true
    }

    /// Retargets a draft across machines while preserving multi-model fan-out selections
    /// (`useDraftEnvironmentRouting.ts:177-188`) and clearing stale source-machine workspace
    /// bindings even when both machines share the same `project_id`.
    pub(super) fn retarget_draft_for_routing(
        &mut self,
        from_project: &str,
        to_project: &str,
        same_env: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = Target::Draft(from_project.to_owned());
        let to = Target::Draft(to_project.to_owned());
        let multi_models = self
            .choices
            .get(&from)
            .map(|c| c.multi_models.clone())
            .unwrap_or_default();
        if from_project != to_project {
            self.retarget_draft(from_project, to_project, same_env, window, cx);
        } else if !same_env {
            self.clear_draft_workspace(to_project, cx);
        }
        if !multi_models.is_empty() {
            self.choices.entry(to).or_default().multi_models = multi_models;
        }
    }

    /// Clears a draft's stored `DraftWorkspace` (for example when switching machines or enabling
    /// automatic routing).
    pub(super) fn clear_draft_workspace(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let target = Target::Draft(project_id.to_owned());
        let storage_key = key(&target);
        if let Some(mut draft) = self.drafts.store.get(&storage_key).cloned()
            && draft.workspace.is_some()
        {
            draft.workspace = None;
            if self.mutate_drafts(cx, |store| store.set(&storage_key, draft)) {
                self.schedule_draft_save(&target, cx);
            }
        }
    }
}

impl Composer {
    pub(crate) fn commit_sidebar_discard(&mut self, discarded: DiscardedDraft, cx: &mut Context<Self>) {
        self.release_discarded_attachments(&discarded.target, &discarded.attachments, cx);
    }

    pub(crate) fn clear_deleted_drafts(&mut self, thread_ids: &[String], cx: &mut Context<Self>) {
        let mut changed = false;
        for id in thread_ids {
            let target = Target::Thread(id.clone());
            let attachments = self.take_draft_attachments(&target);
            if !attachments.is_empty() {
                self.commit_sidebar_discard(DiscardedDraft { target: target.clone(), draft: Draft::default(), attachments }, cx);
                changed = true;
            } else {
                self.release_target_uploads(&target, cx);
            }
            let storage_key = key(&target);
            let had_draft = self.drafts.store.get(&storage_key).is_some();
            if had_draft || self.choices.contains_key(&target) {
                self.discard_draft(&target, cx);
                changed = true;
            }
            self.clear_mentions(&target);
            self.context.records.remove(&target);
            self.context.reviews.remove(&target);
            self.context.reply_to.remove(&target);
            self.clear_terminals(&target);
        }
        if changed {
            self.drafts.pruned.clear();
            broadcast_draft_events(&Target::Thread(String::new()), &[], cx.entity_id(), cx);
            save_shared_now(cx);
            cx.notify();
        }
    }

    pub(crate) fn seed_model_selection(&mut self, thread_id: &str, selection: t3_core::model::ModelSelection, cx: &mut Context<Self>) {
        let target = Target::Thread(thread_id.to_owned());
        let storage_key = key(&target);
        let mut draft = self.drafts.store.get(&storage_key).cloned().unwrap_or_default();
        draft.model_selection = Some(selection);
        draft.updated_at = now_ms();
        self.restore_draft_choices(&target, &draft, cx);
        if self.mutate_drafts(cx, |store| store.set(&storage_key, draft)) {
            let pruned = std::mem::take(&mut self.drafts.pruned);
            broadcast_draft_events(&target, &pruned, cx.entity_id(), cx);
            save_shared_now(cx);
        }
        cx.notify();
    }
}

pub(crate) fn seed_side_chat_model_selection(thread_id: &str, selection: t3_core::model::ModelSelection, cx: &mut gpui::App) {
    let live = live_composers(cx);
    if let Some(first) = live.first() {
        first.update(cx, |composer, cx| composer.seed_model_selection(thread_id, selection.clone(), cx));
        for composer in live.into_iter().skip(1) {
            let mut draft = load_shared_store(cx).get(&rules::thread_key(thread_id)).cloned().unwrap_or_default();
            draft.model_selection = Some(selection.clone());
            composer.update(cx, |composer, cx| {
                composer.restore_draft_choices(&Target::Thread(thread_id.to_owned()), &draft, cx);
            });
        }
        return;
    }
    let _ = load_shared_store(cx);
    let storage_key = rules::thread_key(thread_id);
    let changed = cx.update_global::<SharedDraftStore, _>(|shared, _| {
        let mut draft = shared.store.get(&storage_key).cloned().unwrap_or_default();
        draft.model_selection = Some(selection);
        draft.updated_at = now_ms();
        shared.store.set(&storage_key, draft)
    });
    if changed {
        save_shared_now(cx);
    }
}

pub(crate) fn clear_deleted_thread_drafts(thread_ids: &[String], cx: &mut gpui::App) {
    if thread_ids.is_empty() {
        return;
    }
    let live = live_composers(cx);
    if live.is_empty() {
        let _ = load_shared_store(cx);
        let changed = cx.update_global::<SharedDraftStore, _>(|shared, _| {
            let mut changed = false;
            for id in thread_ids {
                changed |= shared.store.remove(&rules::thread_key(id));
            }
            changed
        });
        if changed {
            save_shared_now(cx);
        }
        return;
    }
    for composer in live {
        composer.update(cx, |composer, cx| composer.clear_deleted_drafts(thread_ids, cx));
    }
}

impl Composer {

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
        if self.mutate_drafts(cx, |store| store.set(&key, draft)) {
            let pruned = std::mem::take(&mut self.drafts.pruned);
            broadcast_draft_events(&target, &pruned, cx.entity_id(), cx);
        }
        cx.notify();
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn set_test_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_prompt_text(text, window, cx);
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn test_prompt(&self, cx: &gpui::App) -> String {
        self.prompt(cx)
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn has_thread_draft(&self, thread_id: &str) -> bool {
        self.drafts.store.get(&key(&Target::Thread(thread_id.to_owned()))).is_some()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn stored_draft_prompt(&self, project_id: &str) -> Option<&str> {
        self.drafts.store.get(&key(&Target::Draft(project_id.to_owned()))).map(|draft| draft.prompt.as_str())
    }
}
