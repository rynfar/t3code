//! The prompt stash (`useComposerStash.ts`, `ComposerStashMenu.tsx`, `ComposerStashBadge.tsx`):
//! Mod+S parks the prompt with its mentions and uploaded attachments, the shoulder tab opens the
//! list, and a restore joins the prompt to what is typed. Also prompt history: Up and Down at the
//! prompt's edges recall what the user sent in this thread (`composerCommandKey.ts`).

use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_client::store;
use t3_core::composer::attachments::Upload;
use t3_core::composer::drafts::{Draft, SavedAttachment};
use t3_core::composer::stash::{self as rules, HistoryPosition, Stash, StashEntry};
use t3_core::sidebar::time::compact_relative_label;

use super::menu::MenuKey;
use super::{Composer, Target};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::sidebar::controller::now_ms;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

#[derive(Default)]
pub(super) struct Stashed {
    store: Stash,
    open: bool,
    highlighted: Option<String>,
    /// The history entry the prompt shows, for the target it was recalled in.
    history: Option<(Target, HistoryPosition)>,
}

impl Composer {
    pub(super) fn load_stash(&mut self) {
        self.stash_state.store = Stash::from_value(store::load_ui_state(rules::STORE_NAME).as_ref());
    }

    fn save_stash(&self) {
        store::save_ui_state(rules::STORE_NAME, &self.stash_state.store.to_value());
    }

    fn toast(&self, kind: ToastKind, title: &str, description: Option<&str>, cx: &mut Context<Self>) {
        let mut toast = Toast::new(kind, title.to_owned());
        if let Some(description) = description {
            toast = toast.description(description.to_owned());
        }
        self.state.update(cx, |state, cx| state.show_toast(toast, cx));
    }

    pub(super) fn toggle_stash_menu(&mut self, cx: &mut Context<Self>) {
        self.stash_state.open = !self.stash_state.open;
        self.stash_state.highlighted = None;
        cx.notify();
    }

    /// The stash closes once the prompt changes under it (`stashMenuClosesOn`).
    pub(super) fn close_stash_menu(&mut self, cx: &mut Context<Self>) {
        if self.stash_state.open {
            self.stash_state.open = false;
            cx.notify();
        }
    }

    /// Mod+S (`handleStashShortcut`, `stashCurrentPrompt`).
    pub(super) fn stash_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        if !self.snapshot.pending.user_inputs.is_empty() {
            self.toggle_stash_menu(cx);
            return;
        }
        if self.editing_queued().is_some() || self.sending {
            return;
        }
        let prompt = self.prompt(cx).trim().to_owned();
        let attachments = self.staged_attachments();
        if prompt.is_empty() && attachments.is_empty() {
            match self.stash_state.store.entries.as_slice() {
                [only] => {
                    let id = only.id.clone();
                    self.restore_stash_entry(&id, window, cx);
                }
                _ => self.toggle_stash_menu(cx),
            }
            return;
        }
        if attachments.iter().any(|attachment| !matches!(attachment.upload, Upload::Ready { .. })) {
            self.toast(ToastKind::Error, "Wait for file uploads before stashing this prompt", None, cx);
            return;
        }
        let entry = StashEntry {
            context_records: t3_core::composer::records::referenced(&prompt,self.records_for(&target)),
            id: uuid_v4(),
            created_at: now_ms(),
            threads: Draft::saved_threads(self.mentions(&target), &prompt),
            terminals: Draft::saved_terminals(self.terminals_for(&target), &prompt),
            review_comments: t3_core::composer::review::referenced(&prompt, self.reviews_for(&target)).into_iter().cloned().collect(),
            attachments: attachments.iter().filter_map(SavedAttachment::of).collect(),
            prompt,
        };
        if let Some(evicted) = self.stash_state.store.push(entry) {
            self.release_saved(&evicted.attachments, cx);
            let description = format!("The stash holds {} prompts; the oldest was removed to make room.", rules::MAX_ENTRIES);
            self.toast(ToastKind::Warning, "Oldest stashed prompt discarded", Some(&description), cx);
        }
        self.save_stash();
        // The attachments now belong to the entry, so they leave without deleting their uploads.
        drop(self.take_attachments(&target));
        self.clear_mentions(&target);
        self.set_prompt("", window, cx);
        self.stash_state.history = None;
        self.draft_changed(cx);
        cx.notify();
    }

    fn release_saved(&self, attachments: &[SavedAttachment], cx: &mut Context<Self>) {
        for attachment in attachments {
            let request = self.state.read(cx).rpc(t3_core::composer::attachments::DELETE, t3_core::composer::attachments::delete_request(&attachment.attachment_id));
            cx.background_executor().spawn(async move { request.await.ok() }).detach();
        }
    }

    /// Restores an entry into the open target: the prompt joins what is typed, its mentions and
    /// attachments come back, and it leaves the stash.
    pub(super) fn restore_stash_entry(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.snapshot.target.clone() else { return };
        let Some(entry) = self.stash_state.store.take(id) else { return };
        self.save_stash();
        let present: Vec<String> = self.staged_for(&target).iter().map(|staged| staged.attachment.id.clone()).collect();
        let restored = entry.attachments.iter().filter(|attachment| !present.contains(&attachment.id)).map(SavedAttachment::restore).collect();
        self.seed_attachments(&target, restored);
        let mut threads = self.mentions(&target).to_vec();
        let stashed = Draft { threads: entry.threads.clone(), ..Draft::default() };
        for thread in stashed.thread_contexts() {
            if !threads.iter().any(|existing| existing.context_id == thread.context_id) {
                threads.push(thread);
            }
        }
        self.seed_mentions(&target, threads);
        self.seed_terminals(&target, entry.terminals.clone());
        self.seed_reviews(&target, entry.review_comments.clone());
        self.seed_records(&target, entry.context_records.clone());
        let prompt = rules::merge_prompt(&self.prompt(cx), &entry.prompt);
        self.set_prompt(&prompt, window, cx);
        let end = prompt.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
        self.stash_state.open = false;
        self.draft_changed(cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn delete_stash_entry(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.stash_state.store.take(id) {
            self.release_saved(&entry.attachments, cx);
            self.save_stash();
            cx.notify();
        }
    }

    /// The open stash list's keys: Up and Down move, Enter restores, Escape closes.
    pub(super) fn stash_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.stash_state.open || self.menu_open() {
            return false;
        }
        let entries = &self.stash_state.store.entries;
        let current = self.stash_state.highlighted.as_ref().and_then(|id| entries.iter().position(|entry| entry.id == *id));
        match key {
            MenuKey::Dismiss => self.stash_state.open = false,
            MenuKey::Up | MenuKey::Down if !entries.is_empty() => {
                let count = entries.len();
                let next = match (current, key == MenuKey::Down) {
                    (None, true) => 0,
                    (None, false) => count - 1,
                    (Some(index), true) => (index + 1) % count,
                    (Some(index), false) => (index + count - 1) % count,
                };
                self.stash_state.highlighted = Some(entries[next].id.clone());
            }
            MenuKey::Accept => {
                let Some(id) = current.or((!entries.is_empty()).then_some(0)).map(|index| entries[index].id.clone()) else { return false };
                self.restore_stash_entry(&id, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// `navigatePromptHistory`: Up on an empty prompt (or a recalled one) at its first line
    /// recalls older prompts, Down at its last line newer ones, then back to empty.
    pub(super) fn history_key(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.snapshot.target.clone() else { return false };
        let Target::Thread(thread_id) = target.clone() else { return false };
        if !self.snapshot.pending.user_inputs.is_empty() || !self.staged().is_empty() || self.editing_queued().is_some() || self.answering.is_some() {
            return false;
        }
        if self.input.update(cx, |input, cx| gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some()) {
            return false;
        }
        let prompt = self.prompt(cx);
        let position = self.stash_state.history.as_ref().filter(|(recalled_in, _)| *recalled_in == target).map(|(_, position)| position.clone());
        if position.is_none() && !prompt.is_empty() {
            return false;
        }
        if !rules::caret_on_edge(&prompt, self.input.read(cx).cursor(), backward) {
            return false;
        }
        let step = {
            let state = self.state.read(cx);
            let Some(projection) = state.thread_state(&thread_id).projection.as_ref().filter(|projection| projection.thread.id == thread_id) else { return false };
            // Built on the keypress: the transcript changes with every streamed delta.
            rules::step_history(backward, &rules::history_entries(projection), position.as_ref(), &prompt)
        };
        let Some(step) = step else { return false };
        self.stash_state.history = step.position.map(|position| (target, position));
        self.set_prompt(&step.prompt, window, cx);
        let end = step.prompt.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
        true
    }

    /// `ComposerStashBadge`: the shoulder tab over the card while anything is stashed.
    pub(super) fn render_stash_badge(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.stash_state.store.entries.len();
        if count == 0 || self.menu_open() {
            return None;
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let open = self.stash_state.open;
        let color = if open { hsla(p.foreground) } else { hsla(p.muted_foreground) };
        let tab = h_flex()
            .id("stash-badge")
            .h(px(28.))
            .px(px(8.))
            .pb(px(4.))
            .gap(px(6.))
            .rounded_t(px(12.))
            .border_1()
            .border_color(if dark { gpui::white().opacity(0.05) } else { hsla(p.foreground).opacity(0.08) })
            .bg(hsla(if dark { p.surface_raised } else { p.card }))
            .text_xs()
            .text_color(color)
            .cursor_pointer()
            .hover(|this| this.text_color(hsla(p.foreground)))
            .child(Icon::Bookmark.render(px(14.), color))
            .child("Stash")
            .child(div().text_color(hsla(p.muted_foreground)).child(count.to_string()))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_stash_menu(cx)));
        Some(h_flex().justify_end().pr(px(22.)).mb(px(-1.)).child(tab).into_any_element())
    }

    /// `ComposerStashMenu`: the stashed prompts as an attached list.
    pub(super) fn render_stash_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.stash_state.open || self.menu_open() {
            return None;
        }
        let p = palette(cx);
        let dark = cx.theme().is_dark();
        let entries = &self.stash_state.store.entries;
        let now = now_ms();
        let header = h_flex()
            .h(px(28.))
            .px(px(4.))
            .gap(px(4.))
            .child(div().w(px(24.)).flex().justify_center().child(Icon::Bookmark.render(px(16.), hsla(p.icon_muted))))
            .child(div().flex_1().text_color(hsla(p.muted_foreground)).child("Stash"))
            .child(div().px(px(6.)).text_color(hsla(p.muted_foreground)).child(entries.len().to_string()))
            .child(close_button("stash-close", cx).on_click(cx.listener(|this, _, _, cx| this.close_stash_menu(cx))));
        let body: AnyElement = if entries.is_empty() {
            div().pl(px(32.)).pr(px(12.)).py(px(6.)).text_color(hsla(p.muted_foreground)).child("Nothing stashed yet. Press ⌘S with a prompt in the composer to stash it.").into_any_element()
        } else {
            v_flex()
                .id("stash-list")
                .max_h(px(384.))
                .overflow_y_scroll()
                .children(entries.iter().map(|entry| {
                    let highlighted = self.stash_state.highlighted.as_deref() == Some(entry.id.as_str());
                    let (restore_id, delete_id, hover_id) = (entry.id.clone(), entry.id.clone(), entry.id.clone());
                    let files = entry.attachments.len();
                    h_flex()
                        .id(SharedString::from(format!("stash-{}", entry.id)))
                        .h(px(28.))
                        .px(px(4.))
                        .gap(px(4.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(highlighted, |this| this.bg(hsla(p.accent)).text_color(hsla(p.accent_foreground)))
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.stash_state.highlighted.as_deref() != Some(hover_id.as_str()) {
                                this.stash_state.highlighted = Some(hover_id.clone());
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| this.restore_stash_entry(&restore_id, window, cx)))
                        .child(div().w(px(24.)).flex().justify_center().child(Icon::FileText.render(px(16.), hsla(p.icon_muted))))
                        .child(div().flex_1().min_w_0().truncate().text_color(hsla(p.foreground).opacity(0.8)).child(entry.snippet()))
                        .when(files > 0, |this| {
                            this.child(
                                h_flex()
                                    .gap(px(4.))
                                    .text_color(hsla(p.muted_foreground))
                                    .child(Icon::File.render(px(12.), hsla(p.muted_foreground)))
                                    .child(files.to_string()),
                            )
                        })
                        .child(div().flex_shrink_0().text_color(hsla(p.muted_foreground)).child(compact_relative_label(entry.created_at, now)))
                        .child(close_button(SharedString::from(format!("stash-delete-{}", entry.id)), cx).on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete_stash_entry(&delete_id, cx);
                            },
                        )))
                }))
                .into_any_element()
        };
        let outline = if dark { gpui::white().opacity(0.05) } else { hsla(p.foreground).opacity(0.08) };
        Some(
            div()
                .mx(px(22.))
                .mb(px(-17.))
                .rounded_t(px(16.))
                .border_1()
                .border_color(outline)
                .bg(hsla(if dark { p.surface_raised } else { p.card }))
                .child(div().pt(px(4.)).pb(px(21.)).px(px(4.)).text_xs().line_height(px(16.)).child(header).child(body))
                .into_any_element(),
        )
    }
}

/// `ComposerBanner.Dismiss`: a small ghost X.
fn close_button(id: impl Into<SharedString>, cx: &gpui::App) -> gpui::Stateful<gpui::Div> {
    let p = palette(cx);
    div()
        .id(gpui::ElementId::Name(id.into()))
        .size(px(20.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|this| this.bg(hsla(p.accent)))
        .child(Icon::X.render(px(12.), hsla(p.muted_foreground)))
}
