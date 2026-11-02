//! Side chats in the right panel: their thread subscriptions beside the open thread's, starting
//! one (`useSideChat.ts`), Bring back, Delete and the tab's close (`SideChatPane.tsx`,
//! `useRightPanelSurfaceClose.ts`), and opening related threads (`openSideChatTab` in
//! `lib/sideChatPane.ts`). Child threads that ask the open thread's user for an approval are
//! subscribed here too, for `ChildThreadApprovals`.

mod pane;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Task};
use t3_core::lineage::latest_merge_back_run;
use t3_core::timeline::child_inputs;
use t3_core::right_panel::Surface;
use t3_core::side_chat::{self, Parent};
use t3_core::thread::ThreadState;

pub use pane::SideChatPane;

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::toast::{Toast, ToastKind};

/// `waitForThreadShell`: how long a new side chat may take to reach the shell.
const SHELL_WAIT: Duration = Duration::from_secs(10);

/// A side chat being started: forked, waiting for the shell to list it, then sent its prompt.
struct Starting {
    parent: String,
    target: String,
    prompt: Option<String>,
    _timeout: Task<()>,
}

/// Thread subscriptions beside the open thread's, keyed by thread id.
#[derive(Default)]
pub struct SideThreads {
    threads: HashMap<String, ThreadState>,
    empty: ThreadState,
    starting: Option<Starting>,
    /// Side chats with a Bring back or Delete in flight.
    pub(crate) busy: Vec<String>,
}

impl SideThreads {
    fn take(&mut self, thread_id: &str) -> Option<ThreadState> {
        self.threads.remove(thread_id)
    }

    pub fn is_starting(&self) -> bool {
        self.starting.is_some()
    }
}

impl AppState {
    /// The detail of the open thread or of a side subscription; empty while it loads.
    pub fn thread_state(&self, thread_id: &str) -> &ThreadState {
        if self.selected_thread.as_deref() == Some(thread_id) {
            return &self.thread;
        }
        self.side_threads.threads.get(thread_id).unwrap_or(&self.side_threads.empty)
    }

    fn thread_state_mut(&mut self, thread_id: &str) -> Option<&mut ThreadState> {
        if self.selected_thread.as_deref() == Some(thread_id) {
            return Some(&mut self.thread);
        }
        self.side_threads.threads.get_mut(thread_id)
    }

    pub fn is_thread_running(&self, thread_id: &str) -> bool {
        self.thread_shell(thread_id).is_some_and(|thread| thread.is_live())
    }

    /// Routes one stream item to a side subscription. Returns whether it changed.
    pub(crate) fn apply_side_thread(&mut self, thread_id: &str, item: t3_core::thread::ThreadStreamItem) -> bool {
        self.side_threads.threads.get_mut(thread_id).is_some_and(|state| state.apply(item))
    }

    /// The open thread's selection moved onto a side subscription: it keeps its live state.
    pub(crate) fn adopt_side_thread(&mut self, thread_id: &str) {
        if let Some(state) = self.side_threads.take(thread_id) {
            self.thread = state;
        }
    }

    /// The side threads the open thread needs: its showing side chat tab and the children that
    /// ask for an approval.
    fn wanted_side_threads(&self) -> Vec<String> {
        let Some(selected) = self.selected_thread.as_deref() else { return Vec::new() };
        let mut wanted: Vec<String> = Vec::new();
        if let Some(id) = self.panels.panels.active_surface(selected).and_then(Surface::side_chat_thread) {
            wanted.push(id.to_owned());
        }
        if let Some(snapshot) = &self.shell.snapshot {
            for child in child_inputs(&snapshot.threads, selected, false).map(|inputs| inputs.children).unwrap_or_default() {
                if child.approval && !wanted.contains(&child.thread_id) {
                    wanted.push(child.thread_id);
                }
            }
        }
        wanted.retain(|id| id != selected);
        wanted
    }

    /// Opens and closes side subscriptions to match what the open thread shows. Also finishes a
    /// side chat start once the shell lists it.
    pub(crate) fn sync_side_threads(&mut self, cx: &mut Context<Self>) {
        self.continue_side_chat_start(cx);
        let wanted = self.wanted_side_threads();
        let client = self.client_handle();
        let selected = self.selected_thread.clone();
        let stale: Vec<String> = self.side_threads.threads.keys().filter(|id| !wanted.contains(id)).cloned().collect();
        for id in stale {
            self.side_threads.threads.remove(&id);
            if let Some(client) = &client
                && selected.as_deref() != Some(id.as_str())
            {
                client.close_thread(&id);
            }
        }
        for id in wanted {
            if !self.side_threads.threads.contains_key(&id) {
                self.side_threads.threads.insert(id.clone(), ThreadState::default());
                if let Some(client) = &client {
                    client.open_thread(&id);
                }
            }
        }
    }

    fn side_chat_parent(&self) -> Option<Parent<'_>> {
        let thread = self.thread_shell(self.selected_thread.as_deref()?)?;
        let runs = self.thread.projection.as_ref().filter(|projection| projection.thread.id == thread.id).map_or(&[][..], |projection| &projection.runs);
        let capability = self.server_config.config.as_ref().map(|config| config.environment.capabilities.side_chats == Some(true));
        Some(Parent::of(thread, runs, capability))
    }

    /// `sideChatsOffered`: whether the open thread shows side chat entry points at all.
    pub fn side_chats_offered(&self) -> bool {
        self.side_chat_parent().is_some_and(|parent| side_chat::offered(&parent))
    }

    /// `sideChatDisabledReason` for the open thread; `None` when one can start now.
    pub fn side_chat_disabled_reason(&self) -> Option<&'static str> {
        match self.side_chat_parent() {
            Some(parent) => side_chat::disabled_reason(&parent),
            None => Some("Side chats start after the first message."),
        }
    }

    /// `useSideChat.open`: forks the open thread's latest stable point, sends `prompt` there,
    /// then shows the side chat's tab. The composer's `/side` and `/btw` call this with the
    /// prompt; the panel launcher without one. A reason it cannot start shows as a toast.
    pub fn open_side_chat(&mut self, prompt: Option<String>, cx: &mut Context<Self>) {
        let Some(parent) = self.selected_thread.clone() else { return };
        if self.side_threads.starting.is_some() {
            return;
        }
        if let Some(reason) = self.side_chat_disabled_reason() {
            self.show_toast(Toast::new(ToastKind::Warning, "Side chat unavailable").description(reason), cx);
            return;
        }
        let target = uuid_v4();
        let prompt = prompt.map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());
        let command = side_chat::fork_command(&uuid_v4(), &parent, &target);
        let timeout_target = target.clone();
        let timeout = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHELL_WAIT).await;
            let _ = this.update(cx, |state, cx| {
                if state.side_threads.starting.as_ref().is_some_and(|starting| starting.target == timeout_target) {
                    state.side_threads.starting = None;
                    state.fail_side_chat_start(&timeout_target, "The side chat was created, but it did not reach this client.", cx);
                }
            });
        });
        self.side_threads.starting = Some(Starting { parent, target: target.clone(), prompt, _timeout: timeout });
        self.dispatch_then(command, None, cx, move |state, ok, cx| {
            if !ok && state.side_threads.starting.as_ref().is_some_and(|starting| starting.target == target) {
                state.side_threads.starting = None;
                state.report_side_chat_error("Could not start a side chat.", cx);
                return;
            }
            state.continue_side_chat_start(cx);
        });
    }

    fn continue_side_chat_start(&mut self, cx: &mut Context<Self>) {
        let listed = self.side_threads.starting.as_ref().is_some_and(|starting| self.shell.thread(&starting.target).is_some());
        if !listed {
            return;
        }
        let Some(Starting { parent, target, prompt, .. }) = self.side_threads.starting.take() else { return };
        // Send before showing the tab: closing a tab whose side chat has no run deletes it.
        match prompt {
            Some(text) => {
                let command = side_chat::first_message_command(&uuid_v4(), &target, &uuid_v4(), &text);
                self.dispatch_then(command, None, cx, move |state, ok, cx| {
                    if ok {
                        state.show_side_chat_tab(&parent, &target, cx);
                    } else {
                        state.fail_side_chat_start(&target, "Could not send the side chat message.", cx);
                    }
                });
            }
            None => self.show_side_chat_tab(&parent, &target, cx),
        }
    }

    /// Drops the empty side chat a failed start leaves behind; one that got its message stays.
    fn fail_side_chat_start(&mut self, target: &str, message: &str, cx: &mut Context<Self>) {
        self.report_side_chat_error(message, cx);
        self.dispatch_then(side_chat::delete_command(&uuid_v4(), target, true), None, cx, |_, _, _| {});
    }

    fn report_side_chat_error(&mut self, message: &str, cx: &mut Context<Self>) {
        self.last_error = Some(message.to_owned());
        cx.emit(AppEvent::Thread);
    }

    fn show_side_chat_tab(&mut self, parent: &str, side_chat: &str, cx: &mut Context<Self>) {
        if self.panels.panels.open(parent, Surface::side_chat(side_chat)) {
            self.save_panels(cx);
        }
    }

    /// `openSideChatTab`, else selects the thread: how related-thread links open.
    pub fn open_related_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let parent = self.selected_thread.clone();
        if let (Some(parent), Some(thread)) = (parent, self.thread_shell(thread_id))
            && side_chat::is_side_chat_of(thread, &parent)
        {
            self.show_side_chat_tab(&parent, thread_id, cx);
            return;
        }
        self.select_thread(thread_id, cx);
    }

    /// Bring back: merges the side chat's latest finished reply into the open thread's next turn.
    pub fn bring_back_side_chat(&mut self, side_chat: &str, cx: &mut Context<Self>) {
        let Some(parent) = self.selected_thread.clone() else { return };
        let parent_active = self.is_thread_running(&parent);
        let Some(run_id) = self.thread_state(side_chat).projection.as_ref().and_then(|projection| latest_merge_back_run(&projection.runs)).map(|run| run.id.clone()) else {
            return;
        };
        if parent_active || self.side_threads.busy.iter().any(|id| id == side_chat) {
            return;
        }
        self.side_threads.busy.push(side_chat.to_owned());
        cx.emit(AppEvent::SideThread);
        let id = side_chat.to_owned();
        let command = t3_core::lineage::merge_back_command(&uuid_v4(), side_chat, &parent, &run_id);
        self.dispatch_then(command, Some("Could not bring back the side chat"), cx, move |state, ok, cx| {
            state.side_threads.busy.retain(|busy| *busy != id);
            if ok {
                state.show_toast(
                    Toast::new(ToastKind::Success, "Brought back").description("The main thread gets this side chat with your next message there."),
                    cx,
                );
            }
            cx.emit(AppEvent::SideThread);
        });
    }

    /// Deletes the side chat (after the caller confirmed) and closes its tab.
    pub fn delete_side_chat(&mut self, side_chat: &str, cx: &mut Context<Self>) {
        let Some(parent) = self.selected_thread.clone() else { return };
        if self.side_threads.busy.iter().any(|id| id == side_chat) {
            return;
        }
        self.side_threads.busy.push(side_chat.to_owned());
        cx.emit(AppEvent::SideThread);
        let id = side_chat.to_owned();
        self.dispatch_then(side_chat::delete_command(&uuid_v4(), side_chat, false), Some("Could not delete the side chat"), cx, move |state, ok, cx| {
            state.side_threads.busy.retain(|busy| *busy != id);
            if ok && state.panels.panels.close_surface(&parent, &Surface::side_chat(&id).id) {
                state.save_panels(cx);
            }
            cx.emit(AppEvent::SideThread);
        });
    }

    /// Closing side chat tabs drops the empty ones: the server keeps a side chat with a run or
    /// message. One the shell no longer has is already deleted.
    pub(crate) fn discard_closed_side_chats(&mut self, closed: &[Surface], cx: &mut Context<Self>) {
        for surface in closed {
            let Some(id) = surface.side_chat_thread() else { continue };
            if self.shell.synchronized && self.shell.thread(id).is_none() {
                continue;
            }
            self.dispatch_then(side_chat::delete_command(&uuid_v4(), id, true), None, cx, |_, _, _| {});
        }
    }

    /// Loads earlier history for a side subscription, as `load_older_history` does for the open thread.
    pub fn load_older_side_history(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client_handle() else { return };
        let Some(cursor) = self.thread_state_mut(thread_id).and_then(ThreadState::begin_history_load) else { return };
        cx.emit(AppEvent::SideThread);
        self.fetch_side_history_page(thread_id.to_owned(), cursor, client, cx);
    }

    fn fetch_side_history_page(&mut self, thread_id: String, cursor: String, client: t3_client::supervisor::ClientHandle, cx: &mut Context<Self>) {
        use t3_core::history::{HistoryStep, PageOutcome};
        let task = {
            let (client, thread_id, cursor) = (client.clone(), thread_id.clone(), cursor.clone());
            self.runtime().spawn(async move { client.history_page(&thread_id, &cursor).await })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
            let started = this.update(cx, |state, cx| {
                let thread = state.side_threads.threads.get_mut(&thread_id)?;
                let outcome = match result {
                    Ok(page) => thread.begin_history_merge(&cursor, page),
                    Err(message) => PageOutcome::Step(thread.fail_history_load(&cursor, message)),
                };
                cx.emit(AppEvent::SideThread);
                match outcome {
                    PageOutcome::Merge(merge) => Some(merge),
                    PageOutcome::Step(_) => None,
                }
            });
            let Ok(Some(merge)) = started else { return };
            let merged = cx.background_executor().spawn(async move { merge.run() }).await;
            let _ = this.update(cx, |state, cx| {
                let Some(thread) = state.side_threads.threads.get_mut(&thread_id) else { return };
                let step = thread.finish_history_merge(merged);
                if step == HistoryStep::Stale {
                    return;
                }
                cx.emit(AppEvent::SideThread);
                if let HistoryStep::Continue(next) = step {
                    state.fetch_side_history_page(thread_id, next, client, cx);
                }
            });
        })
        .detach();
    }
}
