//! Queued messages above the composer (`QueuedRunsControl.tsx`): dashed rows that send now,
//! open in the composer for editing (`useQueuedRunEdit.ts`, text only, so the queued attachments
//! stay), or leave the queue. Queue state and commands come from `t3_core::composer::queue`.

use std::collections::HashSet;

use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::composer::attachments::text_for_send;
use t3_core::composer::queue::{self as rules, QueuedRun};
use t3_core::timeline::Attribution;

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
    /// Expanded `Automation` / `Agent` queued rows by `run_id`.
    expanded: HashSet<String>,
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

    /// `steerNext`: steer or send the first queued run if eligible (`ChatView.tsx`).
    pub(crate) fn steer_queued_message(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if matches!(self.snapshot.target, Some(super::Target::Draft(_))) || self.queue.busy.is_some() || self.is_rewinding(cx) {
            return false;
        }
        let queue = self.snapshot.queue.clone();
        let Some(next) = queue.queued.first() else {
            return false;
        };
        if !queue.can_send_run(next.fresh_context) {
            return false;
        }
        self.send_queued_now(next.run_id.clone(), cx);
        true
    }

    /// `editLatest`: load the last editable queued run into the composer when caret is at start (`ChatView.tsx`).
    pub(crate) fn edit_queued_message(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if matches!(self.snapshot.target, Some(super::Target::Draft(_)))
            || self.queue.editing.is_some()
            || self.queue.busy.is_some()
            || self.is_rewinding(cx)
            || !self.snapshot.pending.user_inputs.is_empty()
        {
            return false;
        }
        let sel = self.input.read(cx).selected_range();
        if sel.start != 0 || sel.end != 0 {
            return false;
        }
        let Some(latest) = self.snapshot.queue.queued.last().cloned() else {
            return false;
        };
        if !latest.editable {
            return false;
        }
        self.begin_queued_edit(&latest, window, cx);
        true
    }

    fn send_queued_now(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if self.queue.busy.is_some() || self.is_rewinding(cx) {
            return;
        }
        let Some(command) = rules::send_now(&self.snapshot.queue, &uuid_v4(), &thread_id, &run_id) else { return };
        self.run_queue_command(run_id, command, "Failed to send the queued message.", cx);
    }

    fn remove_queued(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if self.queue.busy.is_some() || self.is_rewinding(cx) {
            return;
        }
        let command = rules::cancel_queued_run(&uuid_v4(), &thread_id, &run_id);
        self.run_queue_command(run_id, command, "Failed to remove the queued message.", cx);
    }

    /// Opens a queued message in the composer, setting the thread's draft aside.
    fn begin_queued_edit(&mut self, run: &QueuedRun, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_rewinding(cx) {
            return;
        }
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
        if self.is_rewinding(cx) {
            return;
        }
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
        let busy = self.queue.busy.is_some() || self.is_rewinding(cx);
        let rows = visible.iter().enumerate().map(|(position, run)| {
            let editing = editing_run == Some(run.run_id.as_str());
            let preview = rules::preview_text(run);
            let images = run.image_count();
            let attribution = run.attribution;
            let is_fold = matches!(attribution, Some(Attribution::Automation | Attribution::Agent));
            let is_automation = attribution == Some(Attribution::Automation);
            let expanded = !is_fold || self.queue.expanded.contains(&run.run_id);
            let app_read = self.state.read(cx);
            let source_thread_id = run
                .linked_sender
                .as_ref()
                .map(|sender| sender.thread_id.clone())
                .or_else(|| run.sender_thread_id.clone());
            let linked_env_id = run.linked_sender.as_ref().map(|sender| sender.environment_id.clone());
            let sender_title = if run.linked_sender.is_none() {
                source_thread_id
                    .as_deref()
                    .and_then(|thread_id| app_read.thread_shell(thread_id))
                    .map(|shell| shell.title.trim().to_owned())
                    .filter(|title| !title.is_empty())
            } else {
                None
            };
            let can_open_sender = attribution == Some(Attribution::Agent)
                && source_thread_id.is_some()
                && app_read.can_read_threads()
                && linked_env_id.as_deref().is_none_or(|env_id| {
                    app_read.environment() == Some(env_id)
                        || app_read.environments.active.as_deref() == Some(env_id)
                        || app_read.fleet.environments.iter().any(|entry| {
                            (entry.summary.owner.saved_id == env_id
                                || entry.summary.owner.canonical_id.as_deref() == Some(env_id))
                                && app_read.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)
                        })
                });
            let _ = app_read;
            let fold_label = if is_automation {
                "Automation".to_owned()
            } else {
                sender_title
                    .or_else(|| run.linked_sender.as_ref().map(|s| s.title.trim().to_owned()).filter(|t| !t.is_empty()))
                    .or_else(|| run.guest_name.clone())
                    .unwrap_or_else(|| "Agent".to_owned())
            };
            let fold_summary = if is_automation {
                preview.lines().next().unwrap_or_default().to_owned()
            } else {
                preview.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().to_owned()
            };
            let border_color = if editing {
                hsla(p.primary).opacity(0.6)
            } else if attribution == Some(Attribution::Guest) {
                hsla(p.guest_foreground).opacity(0.5)
            } else {
                hsla(p.muted_foreground).opacity(0.4)
            };
            let run_for_toggle = run.run_id.clone();
            let fold_header = is_fold.then(|| {
                let icon = if is_automation { Icon::CalendarClock } else { Icon::MessagesSquare };
                let chevron = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
                h_flex()
                    .id(SharedString::from(format!("queued-fold-{position}")))
                    .relative()
                    .debug_selector(move || format!("queued-fold-{position}"))
                    .w_full()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .cursor_pointer()
                    .hover(|this| this.text_color(hsla(p.foreground)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.queue.expanded.remove(&run_for_toggle) {
                            this.queue.expanded.insert(run_for_toggle.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap(px(6.))
                            .child(icon.render(px(13.), hsla(p.muted_foreground)))
                            .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(SharedString::from(fold_label)))
                            .when(!expanded && !fold_summary.is_empty(), |this| {
                                this.child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_xs()
                                        .text_color(hsla(p.muted_foreground))
                                        .child(SharedString::from(fold_summary)),
                                )
                            }),
                    )
                    .child(chevron.render(px(14.), hsla(p.muted_foreground)))
            });
            let bubble = v_flex()
                .relative()
                .block_mouse_except_scroll()
                .min_w_0()
                .max_w(relative(0.8))
                .when(is_fold && expanded, |this| this.w_full())
                .rounded(px(16.))
                .border_1()
                .border_dashed()
                .border_color(border_color)
                .text_sm()
                .text_color(if editing { hsla(p.foreground) } else { hsla(p.muted_foreground) })
                .when(!is_fold, |this| this.px(px(12.)).py(px(8.)).gap(px(8.)))
                .child(crate::glass::backdrop(16., hsla(p.composer_surface), gpui::transparent_black(), cx))
                .children(fold_header)
                .when(expanded, |this| {
                    let body = v_flex()
                        .relative()
                        .gap(px(8.))
                        .when(is_fold, |this| this.px(px(12.)).pb(px(8.)))
                        .when(images > 0, |this| {
                            this.child(h_flex().flex_wrap().gap(px(4.)).children((0..images).map(|_| {
                                div().size(px(32.)).rounded(px(6.)).border_1().border_color(hsla(p.border).opacity(0.7)).bg(hsla(p.muted).opacity(0.6))
                            })))
                        })
                        .when(!preview.is_empty(), |this| this.child(div().whitespace_normal().child(SharedString::from(preview))));
                    this.child(body)
                });
            let run_for_send = run.run_id.clone();
            let run_for_remove = run.run_id.clone();
            let run_for_edit = run.clone();
            let steer_shortcut = (position == 0).then(|| crate::keymap::label("thread.steerQueuedMessage", cx)).flatten();
            let send_tooltip = steer_shortcut.map_or_else(|| queue.send_tooltip().to_owned(), |shortcut| format!("{} ({shortcut})", queue.send_tooltip()));
            let edit_shortcut = (position + 1 == queue.queued.len())
                .then(|| crate::keymap::label_in("thread.editQueuedMessage", &t3_core::keybindings::ShortcutMatchContext::default().with("composerFocus", true), cx))
                .flatten();
            let edit_tooltip = blocked
                .map(str::to_owned)
                .unwrap_or_else(|| edit_shortcut.map_or_else(|| "Edit in the composer".to_owned(), |shortcut| format!("Edit in the composer ({shortcut})")));
            let can_send_this_run = queue.can_send_run(run.fresh_context);
            let open_thread_id = source_thread_id.clone();
            let open_thread_env = linked_env_id.clone();
            let open_task_id = run.scheduled_task_id.clone();
            let controls = h_flex()
                .block_mouse_except_scroll()
                .gap(px(2.))
                .pr(px(4.))
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .when_some(open_thread_id.filter(|_| can_open_sender), |this, tid| {
                    let env_id = open_thread_env.clone();
                    this.child(
                        Button::new(("queued-open-thread", position), ButtonVariant::Ghost, ButtonSize::Xs)
                            .icon(Icon::ExternalLink)
                            .label("Open thread")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let tid = tid.clone();
                                let env_id = env_id.clone();
                                this.state.update(cx, |s, cx| {
                                    if let Some(eid) = env_id.as_deref()
                                        && s.environment() != Some(eid)
                                        && s.environments.active.as_deref() != Some(eid)
                                    {
                                        if let Some(owner) = s
                                            .fleet
                                            .environments
                                            .iter()
                                            .find(|e| e.summary.owner.saved_id == eid || e.summary.owner.canonical_id.as_deref() == Some(eid))
                                            .map(|e| e.summary.owner.clone())
                                        {
                                            s.navigate_scoped(owner, crate::app_state::ScopedRouteAction::Thread(tid), cx);
                                        }
                                    } else {
                                        s.open_related_thread(&tid, cx);
                                    }
                                });
                            })),
                    )
                })
                .when_some(open_task_id, |this, task_id| {
                    this.child(
                        Button::new(("queued-open-automation", position), ButtonVariant::Ghost, ButtonSize::Xs)
                            .icon(Icon::ExternalLink)
                            .label("Open automation")
                            .on_click(move |_, _, cx| crate::settings::open_scheduled_task(&task_id, cx)),
                    )
                })
                .when(can_send_this_run && !editing, |this| {
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
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.is_rewinding(cx) {
                                        return;
                                    }
                                    this.cancel_queued_edit(window, cx);
                                })),
                        )
                    } else {
                        this.child(
                            Button::new(("queued-edit", position), ButtonVariant::Ghost, ButtonSize::IconXs)
                                .icon(Icon::Pencil)
                                .tooltip(edit_tooltip)
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
            v_flex()
                .w_full()
                .items_end()
                .gap(px(4.))
                .when(attribution == Some(Attribution::T3Code), |this| {
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child("Sent by T3 Code"))
                })
                .when_some(run.guest_name.as_ref().filter(|_| attribution == Some(Attribution::Guest)).cloned(), |this, name| {
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.guest_foreground)).child(SharedString::from(name)))
                })
                .child(bubble)
                .child(controls)
        });
        let toggle = collapsible.then(|| {
            let label = if self.queue.show_all { "Show fewer".to_owned() } else { format!("Show all {} queued", queue.queued.len()) };
            h_flex()
                .id("queued-show-all")
                .block_mouse_except_scroll()
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
