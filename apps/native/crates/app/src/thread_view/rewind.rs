//! "Edit from here" (`useTimelineRewind.ts`, `RevertConfirmHost` in `ChatOverlayHosts.tsx`):
//! which user messages can rewind, the confirmation, the `checkpoint.rollback` dispatch, the wait
//! for the projection to show it landed, and the prompt handed back to the composer. Restoring the
//! message's attachments into the composer is not ported.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, BoxShadow, Context, FocusHandle, FontWeight, IntoElement, KeyDownEvent, MouseButton, ParentElement, SharedString, Styled, Task, Window, anchored,
    deferred, div, hsla as gpui_hsla, point, prelude::*, px,
};
use t3_core::thread::Visibility;
use t3_core::timeline::{
    CONFIRM_DESCRIPTION, CONFIRM_TITLE, INTERRUPT_FIRST, KEEP_CHANGES, NO_ROLLBACK, REVERT_FILES, REWIND_FAILED, REWIND_TIMED_OUT, REWIND_TIMEOUT_SECS, RevertInput,
    RewindProgress, RewindWatch, available_revert_turn_counts, find_revert_checkpoint, recallable_prompt, revert_turn_counts, rewind_progress, rollback_command,
    unsettled_run_id, watch_rewind,
};

use super::ThreadView;
use crate::theme::{hsla, palette};

struct Confirm {
    message_id: String,
    turn_count: u64,
    text: String,
    restore_focus: Option<FocusHandle>,
}

struct Active {
    watch: RewindWatch,
    text: String,
    accepted: bool,
    _timeout: Task<()>,
}

#[derive(Default)]
pub(super) struct RewindUi {
    /// The turn count each user message rewinds to, for the messages that can.
    pub counts: HashMap<String, u64>,
    confirm: Option<Confirm>,
    active: Option<Active>,
    focus: Option<FocusHandle>,
}

impl RewindUi {
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
}

impl ThreadView {
    /// `deriveRevertTurnCountByUserMessageId` with the web's inputs. While a turn runs the
    /// buttons are disabled anyway, so the counts are left as they were rather than re-derived
    /// on every streamed token.
    pub(super) fn sync_revert_counts(&mut self, cx: &mut Context<Self>) {
        if self.activity.is_working && !self.rewind.counts.is_empty() {
            return;
        }
        let state = self.state.read(cx);
        let Some(projection) = state.thread.projection.as_ref() else {
            self.rewind.counts.clear();
            return;
        };
        let rollback = state.server_config.config.as_ref().and_then(|config| {
            config.providers.iter().find(|provider| provider.instance_id == projection.thread.provider_instance_id).map(|provider| provider.supports_conversation_rollback)
        });
        if rollback.is_none() || rollback.flatten() == Some(false) {
            self.rewind.counts.clear();
            return;
        }
        let available = available_revert_turn_counts(projection);
        let inherited: HashSet<String> =
            projection.visible_turn_items.iter().filter(|row| row.visibility == Visibility::Inherited).map(|row| row.item.id.clone()).collect();
        let unsettled = unsettled_run_id(&self.activity);
        let input = RevertInput {
            entries: self.entries_cache.entries(),
            checkpoints: &projection.checkpoints,
            thread_start_ready: !state.thread.history.has_more_history && find_revert_checkpoint(&projection.checkpoints, 0).is_some(),
            available: &available,
            unsettled_run_id: unsettled.as_deref(),
            inherited: &inherited,
        };
        self.rewind.counts = revert_turn_counts(&input);
    }

    /// `onRevertToTurnCount` before the confirmation: the guards, then the dialog.
    pub fn request_rewind(&mut self, message_id: &str, turn_count: u64, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewind.busy() {
            return;
        }
        if !self.rewind.counts.contains_key(message_id) {
            self.show_local_error(NO_ROLLBACK.to_owned(), cx);
            return;
        }
        if self.activity.is_working {
            self.show_local_error(INTERRUPT_FIRST.to_owned(), cx);
            return;
        }
        let focus = self.rewind.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let restore_focus = window.focused(cx);
        self.rewind.confirm = Some(Confirm { message_id: message_id.to_owned(), turn_count, text: text.to_owned(), restore_focus });
        window.focus(&focus, cx);
        cx.notify();
    }

    fn cancel_rewind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.rewind.confirm.take().and_then(|confirm| confirm.restore_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// `rewindToMessage`: dispatch the rollback, then wait for the projection to show it.
    fn confirm_rewind(&mut self, restore_files: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Confirm { message_id, turn_count, text, restore_focus }) = self.rewind.confirm.take() else { return };
        if let Some(focus) = restore_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
        if !self.rewind.counts.contains_key(&message_id) {
            return;
        }
        let state = self.state.read(cx);
        let Some(projection) = state.thread.projection.as_ref() else { return };
        let Some(checkpoint) = find_revert_checkpoint(&projection.checkpoints, turn_count) else {
            self.show_local_error(REWIND_FAILED.to_owned(), cx);
            return;
        };
        let command_id = crate::app_state::uuid_v4();
        let watch = match watch_rewind(projection, turn_count, &command_id) {
            Ok(watch) => watch,
            Err(message) => {
                self.show_local_error(message, cx);
                return;
            }
        };
        let request = state.rpc(t3_core::commands::method::DISPATCH_COMMAND, rollback_command(&command_id, &watch.thread_id, &checkpoint, restore_files));
        self.local_error = None;
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(REWIND_TIMEOUT_SECS)).await;
            let _ = this.update(cx, |view, cx| {
                if view.rewind.active.take().is_some() {
                    view.show_local_error(REWIND_TIMED_OUT.to_owned(), cx);
                    cx.notify();
                }
            });
        });
        self.rewind.active = Some(Active { watch, text, accepted: false, _timeout: timeout });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |view, cx| {
                let Some(active) = view.rewind.active.as_mut().filter(|active| active.watch.request_id == command_id) else { return };
                match result {
                    Ok(_) => {
                        active.accepted = true;
                        view.check_rewind(cx);
                    }
                    Err(error) => {
                        view.rewind.active = None;
                        let message = error.to_string();
                        view.show_local_error(if message.trim().is_empty() { REWIND_FAILED.to_owned() } else { message }, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Called on every projection change while a rewind is out.
    pub(super) fn check_rewind(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.rewind.active.as_ref() else { return };
        let state = self.state.read(cx);
        let Some(projection) = state.thread.projection.as_ref().filter(|projection| projection.thread.id == active.watch.thread_id) else { return };
        match rewind_progress(&active.watch, projection) {
            RewindProgress::Failed(message) => {
                self.rewind.active = None;
                self.show_local_error(message, cx);
            }
            RewindProgress::Done if active.accepted => {
                let Some(active) = self.rewind.active.take() else { return };
                let prompt = recallable_prompt(&active.text);
                let thread_id = active.watch.thread_id;
                self.state.update(cx, |state, cx| state.restore_composer_prompt(&thread_id, prompt, cx));
            }
            _ => return,
        }
        cx.notify();
    }

    /// Leaving the thread drops a pending confirmation; a rewind already sent keeps running on
    /// the server, but nothing here waits for it.
    pub(super) fn reset_rewind(&mut self) {
        self.rewind.counts.clear();
        self.rewind.confirm = None;
        self.rewind.active = None;
    }

    /// `RevertConfirmHost`: an alert dialog with Cancel, "Revert files too" and "Revert and keep
    /// changes". Escape cancels; Enter keeps changes, the dialog's primary action.
    pub(super) fn render_rewind_confirm(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.rewind.confirm.as_ref()?;
        let focus = self.rewind.focus.clone()?;
        let p = palette(cx);
        let viewport = window.viewport_size();
        let button = |id: &'static str, label: &'static str, fill: u32, text: u32, border: u32| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(border))
                .bg(hsla(fill))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(text))
                .cursor_pointer()
                .hover(|this| this.opacity(0.9))
                .child(SharedString::from(label))
        };
        let dialog = div()
            .id("rewind-confirm")
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                "escape" => this.cancel_rewind(window, cx),
                "enter" => this.confirm_rewind(false, window, cx),
                _ => {}
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w_full()
            .max_w(px(512.))
            .relative()
            .rounded(px(16.))
            .child(crate::glass::backdrop(16., hsla(p.dialog_surface), hsla(p.dialog_border), cx))
            .shadow(vec![BoxShadow { color: gpui_hsla(0., 0., 0., 0.65), offset: point(px(0.), px(24.)), blur_radius: px(64.), spread_radius: px(-24.), inset: false }])
            .child(
                div()
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_xl().line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(p.popover_foreground)).child(CONFIRM_TITLE))
                    .child(div().text_sm().text_color(hsla(p.muted_foreground)).child(CONFIRM_DESCRIPTION)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(24.))
                    .py(px(16.))
                    .border_t_1()
                    .border_color(hsla(p.border))
                    .rounded_b(px(15.))
                    .bg(hsla(p.muted).opacity(0.72))
                    .child(button("rewind-cancel", "Cancel", p.popover, p.foreground, p.input).on_click(cx.listener(|this, _, window, cx| this.cancel_rewind(window, cx))))
                    .child(
                        button("rewind-revert-files", REVERT_FILES, p.popover, p.destructive_foreground, p.input)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rewind(true, window, cx))),
                    )
                    .child(
                        button("rewind-keep-changes", KEEP_CHANGES, p.primary, p.primary_foreground, p.primary)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_rewind(false, window, cx))),
                    ),
            );
        let backdrop = div()
            .id("rewind-confirm-backdrop")
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .bg(hsla(p.dialog_backdrop))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.cancel_rewind(window, cx)))
            .child(dialog);
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(backdrop)).with_priority(50).into_any_element())
    }
}
