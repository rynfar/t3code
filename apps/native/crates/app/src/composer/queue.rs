//! Queued messages above the composer (`QueuedRunsControl.tsx`): dashed rows that send now,
//! open in the composer for editing (`useQueuedRunEdit.ts`, text only, so the queued attachments
//! stay), or leave the queue. Queue state and commands come from `t3_core::composer::queue`.

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::composer::attachments::text_for_send;
use t3_core::composer::queue::{self as rules, QueuedRun};

use super::Composer;
use super::controls::{Button, ButtonSize, ButtonVariant};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// A queued message open in the composer.
#[derive(Debug, Clone)]
pub(super) struct Editing {
    pub(super) thread_id: String,
    pub(super) run_id: String,
    original: String,
    has_attachments: bool,
    /// The thread's own draft, back in the prompt when the edit ends.
    pub(super) stash: String,
}

#[derive(Default)]
pub(super) struct Queue {
    show_all: bool,
    /// The run a send-now or remove is in flight for.
    busy: Option<String>,
    pub(super) editing: Option<Editing>,
}

impl Composer {
    pub(super) fn editing_queued(&self) -> Option<&Editing> {
        self.queue.editing.as_ref()
    }

    fn run_queue_command(&mut self, run_id: String, command: serde_json::Value, failure: &'static str, cx: &mut Context<Self>) {
        self.queue.busy = Some(run_id);
        let request = self.state.read(cx).rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.queue.busy = None;
                if let Err(error) = result {
                    this.error = Some(format!("{failure} {error}"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn send_queued_now(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if self.queue.busy.is_some() {
            return;
        }
        let Some(command) = rules::send_now(&self.snapshot.queue, &uuid_v4(), &thread_id, &run_id) else { return };
        self.run_queue_command(run_id, command, "Failed to send the queued message.", cx);
    }

    fn remove_queued(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if self.queue.busy.is_some() {
            return;
        }
        let command = rules::cancel_queued_run(&uuid_v4(), &thread_id, &run_id);
        self.run_queue_command(run_id, command, "Failed to remove the queued message.", cx);
    }

    /// Opens a queued message in the composer, setting the thread's draft aside.
    fn begin_queued_edit(&mut self, run: &QueuedRun, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if !run.editable || rules::edit_blocked_reason(self.snapshot.pending.user_inputs.len()).is_some() {
            return;
        }
        let stash = match self.queue.editing.take() {
            Some(editing) => editing.stash,
            None => self.prompt(cx),
        };
        let text = rules::editable_text(&run.text);
        self.queue.editing =
            Some(Editing { thread_id, run_id: run.run_id.clone(), original: text.clone(), has_attachments: !run.attachments.is_empty(), stash });
        self.queue.show_all = true;
        self.set_prompt(&text, window, cx);
        let end = text.len();
        self.input.update(cx, |input, cx| input.set_selected_range(end..end, cx));
        self.focus(window, cx);
        cx.notify();
    }

    /// Ends the edit and puts the thread's draft back.
    pub(super) fn cancel_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editing) = self.queue.editing.take() {
            self.set_prompt(&editing.stash, window, cx);
            cx.notify();
        }
    }

    /// `resolveQueuedEditExit` once the edited run left the queue: an unchanged edit just ends;
    /// a changed one stays as the draft when the draft was empty, else the draft comes back.
    pub(super) fn sync_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = &self.queue.editing else { return };
        let still_queued = self.thread_id().as_deref() == Some(editing.thread_id.as_str())
            && self.snapshot.queue.queued.iter().any(|run| run.run_id == editing.run_id);
        if still_queued || !self.snapshot.loaded {
            return;
        }
        let Some(editing) = self.queue.editing.take() else { return };
        let changed = self.prompt(cx).trim() != editing.original.trim();
        if !changed || !editing.stash.trim().is_empty() {
            self.set_prompt(&editing.stash, window, cx);
        }
        cx.notify();
    }

    /// Send while editing saves the queued message in place.
    pub(super) fn save_queued_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.queue.editing.clone() else { return };
        let prompt = self.prompt(cx).trim().to_owned();
        if prompt.is_empty() && !editing.has_attachments {
            return;
        }
        let text = text_for_send(&prompt, editing.has_attachments);
        let command = rules::edit_queued_run(&uuid_v4(), &editing.thread_id, &editing.run_id, &text);
        let request = self.state.read(cx).rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
        self.sending = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(_) => {
                        if this.queue.editing.as_ref().is_some_and(|current| current.run_id == editing.run_id) {
                            this.cancel_queued_edit(window, cx);
                        }
                    }
                    Err(error) => this.error = Some(format!("Could not save the edited queued message. {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let queue = &self.snapshot.queue;
        if queue.queued.is_empty() {
            return None;
        }
        let p = palette(cx);
        let collapsible = queue.queued.len() > rules::VISIBLE_ROWS;
        let visible = if collapsible && !self.queue.show_all { &queue.queued[..rules::VISIBLE_ROWS - 1] } else { &queue.queued[..] };
        let blocked = rules::edit_blocked_reason(self.snapshot.pending.user_inputs.len());
        let editing_run = self.queue.editing.as_ref().map(|editing| editing.run_id.as_str());
        let busy = self.queue.busy.is_some();
        let rows = visible.iter().enumerate().map(|(position, run)| {
            let editing = editing_run == Some(run.run_id.as_str());
            let preview = rules::preview_text(run);
            let images = run.image_count();
            let bubble = v_flex()
                .min_w_0()
                .max_w(relative(0.8))
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(16.))
                .border_1()
                .border_dashed()
                .border_color(if editing { hsla(p.primary).opacity(0.6) } else { hsla(p.muted_foreground).opacity(0.4) })
                .text_sm()
                .text_color(if editing { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                .when(images > 0, |this| {
                    this.child(h_flex().flex_wrap().gap(px(4.)).children((0..images).map(|_| {
                        div().size(px(32.)).rounded(px(6.)).border_1().border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.muted).opacity(0.6))
                    })))
                })
                .when(!preview.is_empty(), |this| this.child(div().whitespace_normal().child(SharedString::from(preview))));
            let run_for_send = run.run_id.clone();
            let run_for_remove = run.run_id.clone();
            let run_for_edit = run.clone();
            let send_tooltip = queue.send_tooltip();
            let controls = h_flex()
                .gap(px(2.))
                .pr(px(4.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .when(queue.can_send() && !editing, |this| {
                    this.child(
                        Button::new(("queued-send", position), ButtonVariant::Ghost, ButtonSize::Xs)
                            .icon(Icon::ArrowUp)
                            .label("Send now")
                            .tooltip(send_tooltip)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| this.send_queued_now(run_for_send.clone(), cx))),
                    )
                })
                .map(|this| {
                    if editing {
                        this.child(div().px(px(4.)).text_color(hsla(p.foreground)).child("Editing in the composer")).child(
                            Button::new(("queued-cancel-edit", position), ButtonVariant::Ghost, ButtonSize::Xs)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| this.cancel_queued_edit(window, cx))),
                        )
                    } else {
                        this.child(
                            Button::new(("queued-edit", position), ButtonVariant::Ghost, ButtonSize::IconXs)
                                .icon(Icon::Pencil)
                                .tooltip(blocked.unwrap_or("Edit in the composer"))
                                .disabled(!run.editable || busy || blocked.is_some())
                                .on_click(cx.listener(move |this, _, window, cx| this.begin_queued_edit(&run_for_edit, window, cx))),
                        )
                        .child(
                            Button::new(("queued-remove", position), ButtonVariant::Ghost, ButtonSize::IconXs)
                                .icon(Icon::X)
                                .tooltip("Remove from queue")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| this.remove_queued(run_for_remove.clone(), cx))),
                        )
                    }
                });
            v_flex().w_full().items_end().gap(px(4.)).child(bubble).child(controls)
        });
        let toggle = collapsible.then(|| {
            let label = if self.queue.show_all { "Show fewer".to_owned() } else { format!("Show all {} queued", queue.queued.len()) };
            h_flex()
                .id("queued-show-all")
                .self_end()
                .pr(px(4.))
                .gap(px(4.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(p.muted_foreground))
                .cursor_pointer()
                .hover(|this| this.text_color(hsla(p.foreground)))
                .child(label)
                .child(if self.queue.show_all { Icon::ChevronUp } else { Icon::ChevronDown }.render(px(14.), hsla(p.muted_foreground)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.queue.show_all = !this.queue.show_all;
                    cx.notify();
                }))
        });
        Some(v_flex().w_full().gap(px(12.)).pb(px(16.)).children(rows).children(toggle).into_any_element())
    }
}
