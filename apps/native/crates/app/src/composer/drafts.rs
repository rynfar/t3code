//! Per-thread drafts (`web/src/composerDraftStore.ts`): switching threads keeps each one's prompt,
//! choices, mentioned threads, terminal excerpts and attachments, and the drafts are written under
//! `T3_NATIVE_HOME` (atomically, debounced) so they survive a restart. The shape lives in
//! `t3_core::composer::drafts`.

use std::time::Duration;

use gpui::{BorrowAppContext, Context, EventEmitter, Task, Window};
use t3_client::store;
use t3_core::composer::drafts::{self as rules, Draft, DraftRef, DraftStore, SavedAttachment};

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

#[cfg(all(test, feature = "snapshot"))]
pub(crate) fn install_test_drafts(value: &serde_json::Value, cx: &mut gpui::App) {
    cx.set_global(SharedDraftStore {
        loaded: true,
        store: DraftStore::from_value(Some(value)),
        save: None,
    });
}

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

pub(crate) fn reusable_session(
    logical_key: &str,
    project: &str,
    active: Option<&DraftRef>,
    mut promoting: impl FnMut(&DraftRef) -> bool,
    cx: &gpui::App,
) -> Option<DraftRef> {
    if let Some(shared) = cx.try_global::<SharedDraftStore>() {
        shared.store.reusable_session(logical_key, project, active, promoting)
    } else {
        active.filter(|draft| !promoting(draft)).cloned()
    }
}

pub(crate) fn remember_session(logical_key: &str, draft: &DraftRef, cx: &mut gpui::App) {
    if cx.try_global::<SharedDraftStore>().is_some_and(|shared| shared.loaded)
        && cx.update_global::<SharedDraftStore, _>(|shared, _| shared.store.remember_session(logical_key, draft))
    {
        schedule_shared_save(cx);
    }
}

pub(crate) fn reset_session_workspace(draft: &DraftRef, cx: &mut gpui::App) {
    if cx.try_global::<SharedDraftStore>().is_some_and(|shared| shared.loaded) {
        cx.update_global::<SharedDraftStore, _>(|shared, _| {
            let key = draft.key();
            if let Some(mut composer) = shared.store.get(&key).cloned() {
                composer.workspace = None;
                shared.store.set(&key, composer);
            }
        });
    }
}

pub(crate) fn remap_empty_session(from: &DraftRef, to: &DraftRef, cx: &mut gpui::App) {
    if cx.try_global::<SharedDraftStore>().is_some_and(|shared| shared.loaded) {
        cx.update_global::<SharedDraftStore, _>(|shared, _| {
            let key = from.key();
            if let Some(mut composer) = shared.store.get(&key).cloned() {
                shared.store.remove(&key);
                composer.workspace = None;
                shared.store.set(&to.key(), composer);
            }
        });
    }
}

pub(super) fn sync_saved_session(composer: &mut Composer, target: Option<&Target>, cx: &mut Context<Composer>) {
    let Some(target @ Target::Draft(_)) = target else { return };
    let Some(shared) = cx.try_global::<SharedDraftStore>() else { return };
    let storage_key = key(target);
    let stored = shared.store.get(&storage_key);
    if stored != composer.drafts.store.get(&storage_key) {
        composer.drafts.store = shared.store.clone();
    }
    if let Some(draft) = stored.filter(|_| !composer.choices.contains_key(target)).cloned() {
        composer.hydrate_draft(target, &draft, cx);
    }
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
        Target::Draft(draft) => cx.emit(DraftsChanged { key: draft.key() }),
        Target::Thread(_) => cx.emit(ThreadDraftsChanged),
        Target::Question(_) => {}
    }
    for key in pruned {
        if DraftRef::from_key(key).is_some() { cx.emit(DraftsChanged { key: key.clone() }); }
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
        Target::Draft(draft) => draft.key(),
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
        .or_else(|| DraftRef::from_key(key).map(Target::Draft))
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
        let initializing = cx.try_global::<SharedDraftStore>().is_none_or(|shared| !shared.loaded);
        let store = load_shared_store(cx);
        if initializing {
            self.state.update(cx, |state, cx| {
                if let Some(draft) = state.active_draft() {
                    state.remember_draft(&draft, cx);
                }
            });
        }
        for key in store.keys() {
            let (Some(target), Some(draft)) = (target(key), store.get(key)) else { continue };
            self.hydrate_draft(&target, draft, cx);
        }
        self.drafts.store = store;
    }

    fn hydrate_draft(&mut self, target: &Target, draft: &Draft, cx: &mut Context<Self>) {
        self.restore_draft_choices(target, draft, cx);
        self.seed_records(target, draft.context_records.clone());
        self.seed_mentions(target, draft.thread_contexts());
        self.seed_terminals(target, draft.terminals.clone());
        self.seed_reviews(target, draft.review_comments.clone());
        self.seed_reply_to(target, draft.reply_to.clone());
        self.publish_review_comments(target, &draft.prompt, cx);
        self.seed_attachments(target, draft.attachments.iter().map(SavedAttachment::restore).collect(), true, cx);
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
        if self.mutate_drafts(cx, |store| {
            let storage_key = key(target);
            let removed = store.remove(&storage_key);
            let unmapped = if let Target::Draft(draft) = target {
                !store.forget_session(draft).is_empty()
            } else {
                false
            };
            removed || unmapped
        }) {
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
        schedule_shared_save(cx);
    }
}

fn schedule_shared_save(cx: &mut gpui::App) {
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(rules::PERSIST_DEBOUNCE_MS)).await;
        cx.update(save_shared_now);
    });
    cx.update_global::<SharedDraftStore, _>(|shared, _| {
        shared.save = Some(task);
    });
}

pub(crate) struct DiscardedDraft {
    target: Target,
    draft: Draft,
    attachments: Vec<super::attachments::Staged>,
    preferred_keys: Vec<String>,
}

pub(crate) struct DraftsChanged { pub key: String }
impl EventEmitter<DraftsChanged> for Composer {}

pub(crate) struct ThreadDraftsChanged;
impl EventEmitter<ThreadDraftsChanged> for Composer {}

impl Composer {
    pub(crate) fn sidebar_drafts(&self) -> &DraftStore { &self.drafts.store }

    pub(crate) fn discard_sidebar_draft(&mut self, draft: &DraftRef, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        if self.state.read(cx).is_promoting_draft(draft) {
            return None;
        }
        self.discard_target_draft(&Target::Draft(draft.clone()), false, window, cx)
    }

    pub(crate) fn discard_thread_draft(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        self.discard_target_draft(&Target::Thread(thread_id.into()), true, window, cx)
    }

    fn discard_target_draft(&mut self, target: &Target, keep_choices: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<DiscardedDraft> {
        if keep_choices || self.snapshot.target.as_ref() == Some(target) {
            self.record_draft(target, cx);
        }
        let storage_key = key(target);
        let draft = cx.global::<SharedDraftStore>().store.get(&storage_key)?.clone();
        let preferred_keys = match target {
            Target::Draft(draft) => self.mutate_drafts(cx, |store| store.forget_session(draft)),
            _ => Vec::new(),
        };
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
        self.context.records.remove(target);
        self.context.reviews.remove(target);
        self.clear_terminals(target);
        self.take_reply_to(target);
        if self.snapshot.target.as_ref() == Some(target) { self.set_prompt("", window, cx); }
        cx.notify();
        Some(DiscardedDraft { target: target.clone(), draft, attachments, preferred_keys })
    }

    pub(crate) fn restore_sidebar_draft(&mut self, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&discarded.target.clone(), discarded, window, cx);
    }

    pub(crate) fn restore_thread_draft(&mut self, thread_id: &str, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_target_draft(&Target::Thread(thread_id.into()), discarded, window, cx);
    }

    fn restore_target_draft(&mut self, target: &Target, discarded: DiscardedDraft, window: &mut Window, cx: &mut Context<Self>) {
        let DiscardedDraft { target: discarded_target, draft, attachments, preferred_keys } = discarded;
        let live = self.current_draft(target, cx);
        if !self.mutate_drafts(cx, |store| {
            if !store.restore(&key(target), draft.clone(), &live) {
                return false;
            }
            if let Target::Draft(draft) = target {
                store.restore_session(draft, &preferred_keys);
            }
            true
        }) {
            self.commit_sidebar_discard(DiscardedDraft { target: discarded_target, draft, attachments, preferred_keys }, cx);
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

    /// Moves the open draft's content when the project creation flow changes its checkout.
    pub(crate) fn retarget_draft(
        &mut self,
        from_project: &str,
        to_project: &str,
        keep_attachments: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.state.read(cx);
        let Some(source) = state.active_draft().filter(|draft| draft.project_id == from_project) else {
            return false;
        };
        let environment = keep_attachments.then_some(state.environments.active.as_deref()).flatten();
        let destination = DraftRef::new(source.id.clone(), environment.unwrap_or_default(), to_project);
        if !self.retarget_non_empty_session(&destination, keep_attachments, window, cx) {
            return false;
        }
        self.state.update(cx, |state, _| state.pending_draft_retarget = Some(destination));
        true
    }

    pub(crate) fn retarget_non_empty_session(
        &mut self,
        destination: &DraftRef,
        keep_attachments: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.snapshot.target.as_ref().is_some_and(|target| self.current_draft(target, cx).has_user_content()) {
            return false;
        }
        self.retarget_session(destination, keep_attachments, window, cx)
    }

    /// Retargets the displayed draft without replacing the destination project's other drafts.
    pub(crate) fn retarget_session(
        &mut self,
        destination: &DraftRef,
        keep_attachments: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Target::Draft(source)) = self.snapshot.target.clone() else { return false; };
        if self.state.read(cx).is_promoting_draft(&source) { return false; }
        let from = Target::Draft(source);
        let to = Target::Draft(destination.clone());
        if from == to {
            return false;
        }
        let mut draft = self.current_draft(&from, cx);
        let multi_models = self.choices.get(&from).map(|choices| choices.multi_models.clone()).unwrap_or_default();
        let source_key = key(&from);
        draft.created_at = self.drafts.store.get(&source_key).and_then(|saved| saved.created_at);
        draft.workspace = None;
        let attachments = if keep_attachments {
            self.take_draft_attachments(&from)
        } else {
            let dropped = self.take_draft_attachments(&from);
            self.commit_sidebar_discard(DiscardedDraft { target: from.clone(), draft: Draft::default(), attachments: dropped, preferred_keys: Vec::new() }, cx);
            draft.attachments.clear();
            draft.pending_attachment_count = 0;
            Vec::new()
        };
        self.discard_draft(&from, cx);
        self.clear_mentions(&from);
        self.context.records.remove(&from);
        self.context.reviews.remove(&from);
        self.clear_terminals(&from);
        self.take_reply_to(&from);
        if self.snapshot.target.as_ref() == Some(&from) {
            self.set_prompt("", window, cx);
        }
        self.context.records.remove(&to);
        self.context.reviews.remove(&to);
        self.clear_terminals(&to);
        self.clear_mentions(&to);
        self.restore_draft_choices(&to, &draft, cx);
        if !multi_models.is_empty() {
            self.choices.entry(to.clone()).or_default().multi_models = multi_models;
        }
        self.seed_reply_to(&to, draft.reply_to.clone());
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
                self.commit_sidebar_discard(DiscardedDraft { target: target.clone(), draft: Draft::default(), attachments, preferred_keys: Vec::new() }, cx);
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
        self.drafts.store.session_drafts()
            .filter(|(session, _)| session.project_id == project_id)
            .max_by_key(|(_, draft)| draft.updated_at)
            .map(|(_, draft)| draft.prompt.as_str())
    }
}
