//! Queued messages above the composer (`QueuedRunsControl.tsx`): dashed rows that send now,
//! open in the composer for editing (`useQueuedRunEdit.ts`), or leave the queue. Queue state and
//! commands come from `t3_core::composer::queue`.

use std::{collections::HashSet, rc::Rc};

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px, relative};
use gpui_component::{h_flex, v_flex};
use t3_core::composer::attachments::text_for_send;
use t3_core::composer::queue::{self as rules, QueuedRun};
use t3_core::media::{self, AssetResource};
use t3_core::timeline::Attribution;

use crate::i18n::{t, t_fmt};
use crate::media::{Media, MediaScope, ViewerItem};

use super::Composer;
use super::controls::{Button, ButtonSize, ButtonVariant};
use crate::app_state::uuid_v4;
use crate::icons::Icon;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};

/// A queued message open in the composer.
#[derive(Debug, Clone)]
pub(super) struct Editing {
    pub(super) thread_id: String,
    pub(super) run_id: String,
    original: String,
    attachments: Vec<serde_json::Value>,
    removed_attachment: bool,
    /// The thread's own draft, back in the prompt when the edit ends.
    pub(super) stash: String,
}

impl Editing {
    pub(super) fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    fn remove_attachment(&mut self, id: &str) {
        let before = self.attachments.len();
        self.attachments.retain(|attachment| attachment["id"].as_str() != Some(id));
        self.removed_attachment |= self.attachments.len() != before;
    }

    fn command(&self, command_id: &str, prompt: &str) -> serde_json::Value {
        let text = text_for_send(prompt, self.has_attachments());
        let mut command = rules::edit_queued_run(command_id, &self.thread_id, &self.run_id, &text);
        if self.removed_attachment {
            command["attachments"] = serde_json::Value::Array(self.attachments.clone());
        }
        command
    }
}

fn open_queued_image(items: &[ViewerItem], index: usize, scope: &MediaScope, window: &mut Window, cx: &mut App) {
    if let Some(viewer) = scope.viewer.upgrade() {
        viewer.update(cx, |viewer, cx| viewer.open_with_scope(items.to_vec(), index, Some(scope.clone()), window, cx));
    }
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

    fn run_queue_command(
        &mut self,
        run_id: String,
        command: serde_json::Value,
        failure: &'static str,
        toast_title: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        self.queue.busy = Some(run_id);
        let request = self.state.read(cx).rpc(t3_core::commands::method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                this.queue.busy = None;
                if let Err(error) = result {
                    let detail = error.to_string();
                    this.error = Some(format!("{failure} {detail}"));
                    if let Some(title) = toast_title {
                        let toast = Toast::new(ToastKind::Error, title).description(detail);
                        this.state.update(cx, |state, cx| state.show_toast(toast, cx));
                    }
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
        self.run_queue_command(
            run_id,
            command,
            "Failed to send the queued message.",
            Some("Could not send queued message"),
            cx,
        );
    }

    fn remove_queued(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else { return };
        if self.queue.busy.is_some() || self.is_rewinding(cx) {
            return;
        }
        let command = rules::cancel_queued_run(&uuid_v4(), &thread_id, &run_id);
        self.run_queue_command(run_id, command, "Failed to remove the queued message.", None, cx);
    }

    /// Opens a queued message in the composer, setting the thread's draft aside.
    fn begin_queued_edit(&mut self, run: &QueuedRun, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending || self.is_rewinding(cx) {
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
        self.queue.editing = Some(Editing {
            thread_id,
            run_id: run.run_id.clone(),
            original: text.clone(),
            attachments: run.attachments.clone(),
            removed_attachment: false,
            stash,
        });
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

    /// Send while editing saves the queued message in place; `Cmd+Enter` (`SubmitIntent::Alternate`)
    /// also steers the edited queued run into the active turn (`pingdotgg/t3code#37507`).
    pub(super) fn save_queued_edit(
        &mut self,
        intent: t3_core::composer::SubmitIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_rewinding(cx) {
            return;
        }
        let Some(editing) = self.queue.editing.clone() else { return };
        let prompt = self.prompt(cx).trim().to_owned();
        if prompt.is_empty() && !editing.has_attachments() {
            return;
        }
        let steer_after = intent == t3_core::composer::SubmitIntent::Alternate && self.snapshot.running;
        let command = editing.command(&uuid_v4(), &prompt);
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
                        if steer_after
                            && this.thread_id().as_deref() == Some(editing.thread_id.as_str())
                            && this.snapshot.queue.queued.iter().any(|run| {
                                run.run_id == editing.run_id && this.snapshot.queue.can_send_run(run.fresh_context)
                            })
                        {
                            this.send_queued_now(editing.run_id.clone(), cx);
                        }
                    }
                    Err(error) => this.error = Some(format!("Could not save the edited queued message. {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn queued_media_scope(&self, cx: &App) -> Option<MediaScope> {
        let scope = cx.try_global::<Media>()?.scope.as_ref()?;
        (scope.thread_id.as_deref() == self.thread_id().as_deref()).then(|| scope.clone())
    }

    fn queued_images(&self, attachments: &[serde_json::Value], removable: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let images = media::image_attachments(attachments);
        if images.is_empty() {
            return None;
        }
        let p = palette(cx);
        let scope = self.queued_media_scope(cx).map(|mut scope| {
            scope.owner = cx.entity_id();
            scope
        });
        let gallery: Rc<[ViewerItem]> = images.iter().map(|image| ViewerItem::image(image.source(), image.name.clone())).collect();
        let edge = px(if removable { 56. } else { 32. });
        let tiles = images.into_iter().enumerate().map(|(index, image)| {
            let label = t_fmt(cx, "Preview {name}", &[("name", image.name.clone().into())]);
            let thumbnail = div()
                .id("preview")
                .size_full()
                .aria_label(label)
                .when_some(scope.clone(), |this, scope| {
                    let gallery = gallery.clone();
                    this.cursor_pointer()
                        .child(crate::media::scoped_image(&image.source(), &scope))
                        .on_click(move |_, window, cx| open_queued_image(&gallery, index, &scope, window, cx))
                });
            let remove = removable.then(|| self.queued_attachment_remove(&image.id, &image.name, cx));
            div()
                .id(SharedString::from(image.id))
                .relative()
                .size(edge)
                .flex_none()
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(p.border).opacity(0.7))
                .bg(hsla(p.muted).opacity(0.6))
                .child(thumbnail)
                .children(remove.map(|remove| div().absolute().top_0().right_0().child(remove)))
        });
        Some(h_flex().flex_wrap().gap(px(4.)).children(tiles).into_any_element())
    }

    fn queued_attachment_remove(&self, id: &str, name: &str, cx: &mut Context<Self>) -> Button {
        let id = id.to_owned();
        Button::new("remove", ButtonVariant::Ghost, ButtonSize::IconXs)
            .icon(Icon::X)
            .tooltip(t_fmt(cx, "Remove {name}", &[("name", name.to_owned().into())]))
            .disabled(self.sending || self.is_rewinding(cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.sending && !this.is_rewinding(cx)
                    && let Some(editing) = &mut this.queue.editing
                {
                    editing.remove_attachment(&id);
                    cx.notify();
                }
            }))
    }

    pub(super) fn render_queued_edit_attachments(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let attachments = &self.queue.editing.as_ref()?.attachments;
        if attachments.is_empty() {
            return None;
        }
        let p = palette(cx);
        let images = self.queued_images(attachments, true, cx);
        let files = attachments.iter().filter_map(|attachment| {
            if media::is_image_attachment(attachment) {
                return None;
            }
            let id = attachment["id"].as_str()?;
            let name = attachment["name"].as_str().filter(|name| !name.is_empty()).unwrap_or_else(|| t(cx, "Attachment"));
            let resource = AssetResource::Attachment { attachment_id: id.to_owned() };
            let label = t_fmt(cx, "Preview {name}", &[("name", name.to_owned().into())]);
            Some(h_flex()
                .id(SharedString::from(id.to_owned()))
                .gap(px(6.))
                .text_xs()
                .child(crate::files::file_icon(name).render(px(16.), hsla(p.muted_foreground)))
                .child(div()
                    .id("open")
                    .flex_1()
                    .truncate()
                    .aria_label(label)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(SharedString::from(name.to_owned()))
                    .on_click(move |_, _, cx| crate::media::open_asset(&resource, cx)))
                .child(self.queued_attachment_remove(id, name, cx)))
        });
        Some(v_flex().id("queued-edit-attachments").gap(px(4.)).mb(px(12.)).children(images).children(files).into_any_element())
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
        let busy = self.sending || self.queue.busy.is_some() || self.is_rewinding(cx);
        let rows: Vec<_> = visible.iter().enumerate().map(|(position, run)| {
            let editing = editing_run == Some(run.run_id.as_str());
            let preview = rules::preview_text(run);
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
                        .when(!editing, |this| this.children(self.queued_images(&run.attachments, false, cx)))
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
                .unwrap_or_else(|| edit_shortcut.map_or_else(
                    || crate::i18n::t(cx, "Edit in the composer").to_owned(),
                    |shortcut| crate::i18n::t_fmt(cx, "Edit in the composer ({shortcut})", &[("shortcut", shortcut.into())]),
                ));
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
                            .label(crate::i18n::t(cx, "Open thread"))
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
                            .label(crate::i18n::t(cx, "Open automation"))
                            .on_click(move |_, _, cx| crate::settings::open_scheduled_task(&task_id, cx)),
                    )
                })
                .when(can_send_this_run && !editing, |this| {
                    this.child(
                        Button::new(("queued-send", position), ButtonVariant::Ghost, ButtonSize::Xs)
                            .icon(Icon::ArrowUp)
                            .label(crate::i18n::t(cx, "Send now"))
                            .tooltip(send_tooltip)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| this.send_queued_now(run_for_send.clone(), cx))),
                    )
                })
                .map(|this| {
                    if editing {
                        this.child(div().px(px(4.)).text_color(hsla(p.foreground)).child(crate::i18n::t(cx, "Editing in the composer"))).child(
                            Button::new(("queued-cancel-edit", position), ButtonVariant::Ghost, ButtonSize::Xs)
                                .label(crate::i18n::t(cx, "Cancel"))
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
                                .tooltip(crate::i18n::t(cx, "Remove from queue"))
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
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.muted_foreground).opacity(0.7)).child(crate::i18n::t(cx, "Sent by T3 Code")))
                })
                .when_some(run.guest_name.as_ref().filter(|_| attribution == Some(Attribution::Guest)).cloned(), |this, name| {
                    this.child(div().mr(px(4.)).text_size(px(11.)).text_color(hsla(p.guest_foreground)).child(SharedString::from(name)))
                })
                .child(bubble)
                .child(controls)
        }).collect();
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


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queued_edit_replaces_attachments_only_after_removal() {
        let image = json!({"type": "image", "id": "image-1", "name": "shot.png"});
        let file = json!({"type": "file", "id": "file-1", "name": "notes.md"});
        let mut editing = Editing {
            thread_id: "thread".into(),
            run_id: "run".into(),
            original: String::new(),
            attachments: vec![image, file.clone()],
            removed_attachment: false,
            stash: String::new(),
        };
        assert!(editing.command("cmd", "edited").get("attachments").is_none());
        editing.remove_attachment("image-1");
        let command = editing.command("cmd", "");
        assert_eq!(command["text"], text_for_send("", true));
        assert_eq!(command["attachments"], json!([file]));
        editing.remove_attachment("file-1");
        assert_eq!(editing.command("cmd", "edited")["attachments"], json!([]));
        assert!(!editing.has_attachments());
    }
}
