//! Composer integration for `copyPromptLink.ts` and `linkedPrompt.ts`.
use super::{Composer, Target};
use crate::toast::{Toast, ToastKind};
use gpui::{App, ClipboardItem, Context, Window};
use t3_core::prompt_links::{self as rules, LinkedPrompt};

gpui::actions!(prompt_links, [CopyPromptLink, CopyPromptLinkWithContext]);
#[derive(Clone, Debug, PartialEq, Eq, gpui::Action)]
#[action(namespace = prompt_links, no_json)]
pub struct ReplaceLinkedPrompt {
    target: Target,
    prompt: LinkedPrompt,
    notes: Vec<String>,
}

impl Composer {
    pub(crate) fn prompt_link(&self, context: bool, cx: &App) -> Option<String> {
        self.snapshot.target.as_ref()?;
        let prompt = self.draft_prompt(cx);
        let state = self.state.read(cx);
        let repo = self.target_project().and_then(|id| state.shell.project(id)).and_then(rules::repository_of);
        if context && repo.is_none() {
            return None;
        }
        let resolved = self.resolve(cx);
        rules::prompt_link_for(
            &prompt,
            context.then_some(repo.as_ref()).flatten(),
            context.then_some(resolved.driver.as_str()).filter(|s| !s.is_empty()),
            context.then_some(resolved.model.as_ref().map(|m| m.model.as_str())).flatten(),
        )
    }
    pub(crate) fn copy_prompt_link(&self, context: bool, cx: &mut Context<Self>) {
        if let Some(link) = self.prompt_link(context, cx) {
            copy(&self.state, link, cx);
        }
    }
    pub(crate) fn place_linked_prompt(&mut self, prompt: LinkedPrompt, notes: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(false, window, cx);
        let Some(target) = self.snapshot.target.clone() else { return };
        warn(&self.state, &prompt, cx);
        if rules::needs_draft_replacement(&self.draft_prompt(cx), &prompt) {
            let replacement = ReplaceLinkedPrompt { target, prompt, notes };
            self.state.update(cx, |state, cx| {
                state.show_toast(
                    Toast::new(ToastKind::Info, "Linked prompt ready")
                        .description("This thread already has a draft. Replace it with the linked prompt?")
                        .replace_linked_prompt(replacement),
                    cx,
                )
            });
        } else {
            self.write_linked_prompt(target, prompt, notes, window, cx);
        }
    }
    pub(crate) fn replace_linked_prompt(&mut self, replacement: &ReplaceLinkedPrompt, window: &mut Window, cx: &mut Context<Self>) {
        self.write_linked_prompt(replacement.target.clone(), replacement.prompt.clone(), replacement.notes.clone(), window, cx);
    }
    fn write_linked_prompt(&mut self, target: Target, prompt: LinkedPrompt, notes: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.snapshot.target.as_ref() == Some(&target) {
            if let Some(editing) = self.queue.editing.as_mut() {
                editing.stash = prompt.text.clone();
            } else if self.answering.is_some() {
                self.stash = Some(prompt.text.clone());
            } else {
                self.set_prompt(&prompt.text, window, cx);
            }
            self.draft_changed(cx);
        } else {
            let key = super::drafts::key(&target);
            let mut draft = self.drafts.store.get(&key).cloned().unwrap_or_default();
            draft.prompt = prompt.text;
            self.drafts.store.set(&key, draft);
            self.schedule_draft_save(&target, cx);
        }
        let description = std::iter::once("Read it before you send.".to_owned())
            .chain(notes)
            .collect::<Vec<_>>()
            .join(" ");
        self.state.update(cx, |state, cx| {
            state.show_toast(Toast::new(ToastKind::Info, "Prompt added from a link").description(description), cx)
        });
    }
    pub(crate) fn linked_model(&mut self, selection: t3_core::model::ModelSelection, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(false, window, cx);
        if let Some(choices) = self.choices_mut() {
            choices.model = Some(selection);
        }
        self.draft_changed(cx);
        cx.notify();
    }
}
#[cfg(feature = "snapshot")]
#[derive(Default)]
pub(crate) struct SnapshotClipboard(pub Option<String>);
#[cfg(feature = "snapshot")]
impl gpui::Global for SnapshotClipboard {}

pub(crate) fn copy(state: &gpui::Entity<crate::app_state::AppState>, link: String, cx: &mut App) {
    // Snapshot copy actions must never touch the user's clipboard.
    #[cfg(feature = "snapshot")]
    if crate::webview::is_offscreen() {
        cx.set_global(SnapshotClipboard(Some(link.clone())));
    }
    if !crate::webview::is_offscreen() {
        cx.write_to_clipboard(ClipboardItem::new_string(link));
    }
    state.update(cx, |state, cx| state.show_toast(Toast::new(ToastKind::Success, "Prompt link copied"), cx));
}
pub(crate) fn warn(state: &gpui::Entity<crate::app_state::AppState>, prompt: &LinkedPrompt, cx: &mut App) {
    let n = prompt.hidden_characters;
    if n == 0 {
        return;
    }
    let suffix = if n == 1 { "" } else { "s" };
    state.update(cx, |state, cx| {
        state.show_toast(
            Toast::new(ToastKind::Warning, "This prompt had hidden characters, it may be malicious")
                .description(format!("T3 Code removed {n} hidden character{suffix}. Read the prompt before you send it."))
                .persistent(),
            cx,
        )
    });
}
