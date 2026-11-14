//! Side chats in the right panel: their thread subscriptions beside the open thread's, starting
//! one (`useSideChat.ts`), Bring back, Delete and the tab's close (`SideChatPane.tsx`,
//! `useRightPanelSurfaceClose.ts`), and opening related threads (`openSideChatTab` in
//! `lib/sideChatPane.ts`). Child threads that ask the open thread's user for an approval are
//! subscribed here too, for `ChildThreadApprovals`.

mod pane;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{Context, Task};
use t3_core::diff::DiffSelection;
use t3_core::lineage::latest_merge_back_run;
use t3_core::timeline::child_inputs;
use t3_core::right_panel::{Surface, SurfaceKind};
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
    runtime_mode: t3_core::server_config::RuntimeMode,
    model_selection: Option<t3_core::model::ModelSelection>,
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

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn insert_for_test(&mut self, id: String, thread: ThreadState) {
        self.threads.insert(id, thread);
    }
}

impl AppState {
    pub(crate) fn can_read_threads(&self) -> bool {
        self.client_handle().is_some_and(|client| client.session_state().borrow().as_ref().is_none_or(|session| session.grants("orchestration:read")))
    }

    pub(crate) fn can_operate_threads(&self) -> bool {
        self.client_handle().is_some_and(|client| {
            client.session_state().borrow().as_ref().is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
        })
    }

    /// The detail of the open thread or of a side subscription; empty while it loads.
    pub fn thread_state(&self, thread_id: &str) -> &ThreadState {
        if self.selected_thread.as_deref() == Some(thread_id) {
            return &self.thread;
        }
        self.side_threads.threads.get(thread_id).unwrap_or(&self.side_threads.empty)
    }

    pub(crate) fn has_side_thread(&self, thread_id: &str) -> bool {
        self.side_threads.threads.contains_key(thread_id)
    }

    pub(crate) fn is_thread_watched(&self, thread_id: &str) -> bool {
        self.selected_thread.as_deref() == Some(thread_id) || self.side_threads.threads.contains_key(thread_id)
    }

    pub(crate) fn thread_state_mut(&mut self, thread_id: &str) -> Option<&mut ThreadState> {
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
        let Some(state) = self.side_threads.threads.get_mut(thread_id) else { return false };
        if state.deleted { return false; }
        let changed = state.apply(item);
        if state.source_deleted() {
            if !state.deleted {
                *state = ThreadState::default();
                state.deleted = true;
            }
            if let Some(client) = self.client_handle() { client.close_thread(thread_id); }
        }
        changed
    }

    /// The open thread's selection moved onto a side subscription: it keeps its live state.
    pub(crate) fn adopt_side_thread(&mut self, thread_id: &str) {
        if let Some(state) = self.side_threads.take(thread_id) {
            self.thread = state;
        }
    }

    /// The visible source and children asking for approval. Tool diffs can read an ancestor
    /// the paged shell has not loaded; side chat panes still require their shell row.
    fn wanted_side_threads(&self) -> Vec<String> {
        let Some(selected) = self.selected_thread.as_deref() else { return Vec::new() };
        let mut wanted: Vec<String> = Vec::new();
        let selection = self.panels.diff_selection(selected);
        if let Some(id) = self.panels.panels.active_thread_source(selected, &selection) {
            let readable = match (&selection, self.panels.panels.active_surface(selected).map(|surface| &surface.kind)) {
                (DiffSelection::ToolCall(target), Some(SurfaceKind::Diff)) => target.source_available(self.thread_shell(id), self.thread_state(id)),
                _ => self.thread_shell(id).is_some() && !self.thread_state(id).deleted,
            };
            if readable && self.can_read_threads() {
                wanted.push(id.to_owned());
            }
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
        let diff_target = selected.as_deref().and_then(|thread| {
            if self.panels.panels.active_surface(thread).is_none_or(|surface| surface.kind != SurfaceKind::Diff) {
                return None;
            }
            match self.panels.diff_selection(thread) {
                DiffSelection::ToolCall(target) => Some(target),
                _ => None,
            }
        });
        let stale: Vec<String> = self.side_threads.threads.keys().filter(|id| !wanted.contains(id)).cloned().collect();
        for id in stale {
            if diff_target.as_ref().is_some_and(|target| target.thread_id == id && !target.source_available(self.thread_shell(&id), self.thread_state(&id))) {
                let state = self.side_threads.threads.get_mut(&id).expect("stale subscription");
                // Keep one deletion tombstone while its diff is active, even across shell snapshots.
                if !state.deleted {
                    *state = ThreadState::default();
                    state.deleted = true;
                    if let Some(client) = &client { client.close_thread(&id); }
                }
                continue;
            }
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
        Some(Parent::of(thread, runs, capability, self.can_operate_threads()))
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
    /// Returns whether the start began.
    pub fn open_side_chat(&mut self, prompt: Option<String>, cx: &mut Context<Self>) -> bool {
        let Some(parent) = self.selected_thread.clone() else { return false };
        if self.side_threads.starting.is_some() || !self.can_operate_threads() {
            return false;
        }
        if let Some(reason) = self.side_chat_disabled_reason() {
            self.show_toast(Toast::new(ToastKind::Warning, "Side chat unavailable").description(reason), cx);
            return false;
        }
        let Some(parent_thread) = self.thread_shell(&parent).cloned() else { return false };
        let (choice_model, choice_runtime) = crate::composer::thread_choices(&parent, cx);
        let (runtime_mode, model_selection) = side_chat::resolve_parent_settings(
            self.server_config.config.as_ref(),
            &parent_thread,
            choice_model.as_ref(),
            choice_runtime,
            self.client_settings.plan_mode_enabled,
        );
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
        self.side_threads.starting = Some(Starting { parent, target: target.clone(), prompt, runtime_mode, model_selection, _timeout: timeout });
        self.dispatch_then(command, None, cx, move |state, ok, cx| {
            if !ok && state.side_threads.starting.as_ref().is_some_and(|starting| starting.target == target) {
                state.side_threads.starting = None;
                state.report_side_chat_error("Could not start a side chat.", cx);
                return;
            }
            state.continue_side_chat_start(cx);
        });
        true
    }

    fn continue_side_chat_start(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.side_threads.starting.as_ref().map(|starting| starting.target.clone()) else {
            return;
        };
        if self.shell.removed_at.contains_key(&target) {
            self.side_threads.starting = None;
            self.fail_side_chat_start(&target, "The side chat is no longer available.", cx);
            return;
        }
        let Some(side_chat_mode) = self.shell.thread(&target).map(|thread| thread.runtime_mode.clone()) else {
            return;
        };
        let Some(Starting { parent, target, prompt, runtime_mode, model_selection, .. }) = self.side_threads.starting.take() else { return };
        if t3_core::server_config::RuntimeMode::parse(&side_chat_mode) != Some(runtime_mode) {
            let mode_command = t3_core::commands::set_runtime_mode(&uuid_v4(), &target, runtime_mode);
            let environment = self.environments.active.clone();
            self.dispatch_then(mode_command, None, cx, move |state, ok, cx| {
                if state.environments.active != environment {
                    return;
                }
                if ok {
                    state.finish_side_chat_start(parent, target, prompt, runtime_mode, model_selection, cx);
                } else {
                    state.fail_side_chat_start(&target, "Could not set the side chat's permission mode.", cx);
                }
            });
            return;
        }
        self.finish_side_chat_start(parent, target, prompt, runtime_mode, model_selection, cx);
    }

    fn finish_side_chat_start(
        &mut self,
        parent: String,
        target: String,
        prompt: Option<String>,
        runtime_mode: t3_core::server_config::RuntimeMode,
        model_selection: Option<t3_core::model::ModelSelection>,
        cx: &mut Context<Self>,
    ) {
        // Send before showing the tab: closing a tab whose side chat has no run deletes it.
        match prompt {
            Some(text) => {
                let command = side_chat::first_message_command(&uuid_v4(), &target, &uuid_v4(), &text, Some(runtime_mode), model_selection.as_ref());
                let environment = self.environments.active.clone();
                self.dispatch_then(command, None, cx, move |state, ok, cx| {
                    if state.environments.active != environment {
                        return;
                    }
                    if ok {
                        state.show_side_chat_tab(&parent, &target, cx);
                    } else {
                        state.fail_side_chat_start(&target, "Could not send the side chat message.", cx);
                    }
                });
            }
            None => {
                if let Some(selection) = model_selection {
                    let seed_target = target.clone();
                    cx.spawn(async move |_, cx| {
                        cx.update(|cx| crate::composer::drafts::seed_side_chat_model_selection(&seed_target, selection, cx));
                    })
                    .detach();
                }
                self.show_side_chat_tab(&parent, &target, cx);
            }
        }
    }

    /// Drops the empty side chat a failed start leaves behind; one that got its message stays.
    fn fail_side_chat_start(&mut self, target: &str, message: &str, cx: &mut Context<Self>) {
        self.report_side_chat_error(message, cx);
        if self.can_operate_threads() {
            self.dispatch_then(side_chat::delete_command(&uuid_v4(), target, true), None, cx, |_, _, _| {});
        }
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
        if !self.can_operate_threads() {
            return;
        }
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
        if !self.can_operate_threads() {
            return;
        }
        let Some(parent) = self.selected_thread.clone() else { return };
        if self.side_threads.busy.iter().any(|id| id == side_chat) {
            return;
        }
        self.side_threads.busy.push(side_chat.to_owned());
        cx.emit(AppEvent::SideThread);
        let id = side_chat.to_owned();
        self.dispatch_then(side_chat::delete_command(&uuid_v4(), side_chat, false), Some("Could not delete the side chat"), cx, move |state, ok, cx| {
            state.side_threads.busy.retain(|busy| *busy != id);
            if ok {
                state.clear_deleted_thread_drafts_later(vec![id.clone()], cx);
                if state.panels.panels.close_surface(&parent, &Surface::side_chat(&id).id) {
                    state.save_panels(cx);
                }
            }
            cx.emit(AppEvent::SideThread);
        });
    }

    pub(crate) fn collect_side_chats_for_deleted_thread(&self, thread_id: &str, out: &mut Vec<String>) {
        if !out.iter().any(|existing| existing == thread_id) {
            out.push(thread_id.to_owned());
        }
        if let Some(snapshot) = &self.shell.snapshot {
            for thread in &snapshot.threads {
                if side_chat::is_side_chat_of(thread, thread_id) && !out.iter().any(|existing| existing == &thread.id) {
                    out.push(thread.id.clone());
                }
            }
        }
        for (id, thread_state) in &self.side_threads.threads {
            let is_child = thread_state.projection.as_ref().is_some_and(|p| {
                p.thread.lineage.side_chat == Some(true)
                    && p.thread.lineage.parent_thread_id.as_deref() == Some(thread_id)
            });
            if is_child && !out.iter().any(|existing| existing == id) {
                out.push(id.clone());
            }
        }
    }

    pub(crate) fn clear_deleted_thread_drafts_later(&self, thread_ids: Vec<String>, cx: &mut Context<Self>) {
        if thread_ids.is_empty() {
            return;
        }
        cx.spawn(async move |_, cx| {
            cx.update(|cx| crate::composer::drafts::clear_deleted_thread_drafts(&thread_ids, cx));
        })
        .detach();
    }

    /// Closing side chat tabs drops the empty ones: the server keeps a side chat with a run or
    /// message. One the shell no longer has is already deleted.
    pub(crate) fn discard_closed_side_chats(&mut self, closed: &[Surface], cx: &mut Context<Self>) {
        if !self.can_operate_threads() {
            return;
        }
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
        let merge_lock = self.history_merge_lock.clone();
        let task = {
            let (client, thread_id, cursor) = (client.clone(), thread_id.clone(), cursor.clone());
            self.runtime().spawn(async move { client.history_page(&thread_id, &cursor).await })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
            let _merge_guard = merge_lock.lock().await;
            let started = this.update(cx, |state, cx| {
                let thread = state.side_threads.threads.get_mut(&thread_id)?;
                let outcome = match result {
                    Ok(page) => thread.begin_history_merge(&cursor, page),
                    Err(message) => PageOutcome::Step(thread.fail_history_load(&cursor, message)),
                };
                match outcome {
                    PageOutcome::Merge(merge) => {
                        cx.emit(AppEvent::SideThread);
                        Some(merge)
                    }
                    PageOutcome::Step(step) => {
                        if step != HistoryStep::Stale {
                            cx.emit(AppEvent::SideThread);
                        }
                        None
                    }
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

#[cfg(all(test, feature = "snapshot"))]
mod history_tests {
    use super::*;
    use std::sync::Arc;

    #[gpui::test]
    async fn side_chat_history_page_serializes_with_deferred_work(cx: &mut gpui::TestAppContext) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let _server = runtime.spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_owned();
                    let item = if path.contains("cursor=work-c1") {
                        serde_json::json!({
                            "position": 0, "visibility": "local", "sourceThreadId": "side-1", "sourceItemId": "w-loaded",
                            "item": {
                                "id": "w-loaded", "threadId": "side-1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 4, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:02.000Z",
                                "input": "echo ok", "output": ""
                            }
                        })
                    } else {
                        serde_json::json!({
                            "position": 0, "visibility": "local", "sourceThreadId": "side-1", "sourceItemId": "m-older",
                            "item": {
                                "id": "m-older", "threadId": "side-1", "runId": null, "nodeId": null, "parentItemId": null,
                                "ordinal": 1, "type": "user_message", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:00.000Z",
                                "messageId": "m-older", "text": "Earlier question", "inputIntent": "message"
                            }
                        })
                    };
                    let body = serde_json::json!({
                        "snapshotSequence": 1,
                        "items": [item],
                        "nextCursor": null,
                        "hasMoreHistory": false,
                        "hasUserTurn": !path.contains("cursor=work-c1")
                    });
                    let json = serde_json::to_string(&body).unwrap();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        });

        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, _fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
            format!("http://127.0.0.1:{port}"),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "side-1", "projectId": "checkout-a", "title": "Side",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"main","parentThreadId":"main"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 2, "visibleItemCount": 2
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": true, "historyCursor": "hist-c1",
                "projection": {
                    "thread": {
                        "id": "side-1", "projectId": "checkout-a", "title": "Side",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"main","parentThreadId":"main"}
                    },
                    "runs": [{
                        "id": "r1", "ordinal": 1, "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "userMessageId": "m0", "rootNodeId": null, "status": "completed",
                        "requestedAt": "2026-04-01T00:00:01.000Z", "startedAt": null,
                        "completedAt": "2026-04-01T00:00:02.000Z"
                    }],
                    "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                    "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                    "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                    "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [
                        {
                            "position": 0, "visibility": "local", "sourceThreadId": "side-1", "sourceItemId": "m0",
                            "item": {
                                "id": "m0", "threadId": "side-1", "runId": null, "nodeId": null, "parentItemId": null,
                                "ordinal": 2, "type": "user_message", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "messageId": "m0", "text": "Side check", "inputIntent": "message"
                            }
                        },
                        {
                            "position": 1, "visibility": "local", "sourceThreadId": "side-1", "sourceItemId": "w-anchor-1",
                            "item": {
                                "id": "w-anchor-1", "threadId": "side-1", "runId": "r1", "nodeId": null, "parentItemId": null,
                                "ordinal": 3, "type": "command_execution", "status": "completed", "title": null,
                                "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                                "input": "ls", "output": "",
                                "deferredWork": { "cursor": "work-c1", "itemCount": 1, "lastOrdinal": 4 }
                            }
                        }
                    ],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            let mut thread = ThreadState::default();
            thread.apply(snapshot);
            state.side_threads.threads.insert("side-1".into(), thread);
            cx.emit(AppEvent::SideThread);
        });

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
        let _sub = state.update(cx, |_, cx| {
            let done_tx = done_tx.clone();
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::SideThread)
                    && let Some(thread) = state.side_threads.threads.get("side-1")
                    && !thread.history.loading
                    && !thread.history.is_loading_work("w-anchor-1")
                    && thread.projection.as_ref().is_some_and(|p| p.has_entry("m-older") && p.has_entry("w-loaded"))
                {
                    if let Some(tx) = done_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        });

        let (view, vcx) = cx.add_window_view(|window, cx| {
            crate::thread_view::ThreadView::embedded(state.clone(), "side-1".into(), window, cx)
        });
        vcx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.toggle_run("r1", cx);
            });
            state.update(cx, |state, cx| {
                state.load_older_side_history("side-1", cx);
            });
        });
        done_rx.await.unwrap();
        state.read_with(cx, |state, _| {
            let thread = state.side_threads.threads.get("side-1").expect("side thread retained");
            let p = thread.projection.as_ref().expect("projection not wiped by unserialized merge");
            assert!(p.has_entry("m-older"));
            assert!(p.has_entry("w-loaded"));
            assert!(!thread.history.loading);
            assert!(!thread.history.is_loading_work("w-anchor-1"));
        });
    }

    #[gpui::test]
    async fn side_chat_start_fails_and_discards_when_target_disappears_after_fork(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        state.update(cx, |state, cx| {
            let mut config = state.server_config.config.clone().unwrap_or_default();
            config.environment.capabilities.side_chats = Some(true);
            config.providers = serde_json::from_value(serde_json::json!([{
                "instanceId": "replay",
                "driver": "replay",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "models": [{"slug": "replay-custom", "name": "Replay Custom"}]
            }]))
            .unwrap();
            state.server_config.config = Some(config);
            state.select_thread("menu-thread", cx);
            assert!(state.open_side_chat(Some("Quick check".into()), cx));
        });

        let target = loop {
            let (method, command, reply) = fixture.next_request().await;
            reply.send(Ok(serde_json::json!({"sequence": 1}))).unwrap();
            if method == t3_core::commands::method::DISPATCH_COMMAND && command["type"] == "thread.fork" {
                break command["targetThreadId"].as_str().unwrap().to_owned();
            }
        };

        state.update(cx, |state, cx| {
            state.shell.removed_at.insert(target.clone(), 2);
            state.sync_side_threads(cx);
        });

        state.read_with(cx, |state, _| {
            assert!(!state.side_threads.is_starting());
            assert_eq!(state.last_error.as_deref(), Some("The side chat is no longer available."));
        });

        let (method, cleanup, reply) = fixture.next_request().await;
        assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
        assert_eq!(cleanup["type"], "thread.delete");
        assert_eq!(cleanup["threadId"], target);
        assert_eq!(cleanup["onlyIfUnused"], true);
        reply.send(Ok(serde_json::json!({"sequence": 2}))).unwrap();
    }

    #[gpui::test]
    async fn side_chat_start_syncs_parent_settings_and_deletion_clears_drafts(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            crate::composer::init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        let side_target = state.update(cx, |state, cx| {
            let mut config = state.server_config.config.clone().unwrap_or_default();
            config.environment.capabilities.side_chats = Some(true);
            config.settings.provider_instances = serde_json::from_value(serde_json::json!({
                "replay": { "driver": "replay", "enabled": true }
            }))
            .unwrap();
            config.providers = serde_json::from_value(serde_json::json!([{
                "instanceId": "replay",
                "driver": "replay",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "models": [{"slug": "replay-custom", "name": "Replay Custom"}]
            }]))
            .unwrap();
            state.server_config.config = Some(config);
            if let Some(shell) = state.shell.snapshot.as_mut()
                && let Some(thread) = shell.threads.iter_mut().find(|t| t.id == "menu-thread")
            {
                thread.runtime_mode = "approval-required".into();
                thread.model_selection = t3_core::model::ModelSelection {
                    instance_id: "replay".into(),
                    model: "replay-custom".into(),
                    options: None,
                };
            }
            state.select_thread("menu-thread", cx);
            assert!(state.open_side_chat(Some("Inspect diff".into()), cx));
            state.side_threads.starting.as_ref().map(|s| s.target.clone()).unwrap()
        });

        loop {
            let (method, cmd, reply) = fixture.next_request().await;
            assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
            reply.send(Ok(serde_json::json!({"sequence": 1}))).unwrap();
            if cmd["type"] == "thread.fork" {
                break;
            }
        }

        state.update(cx, |state, cx| {
            let mut shell = serde_json::to_value(state.thread_shell("menu-thread").unwrap()).unwrap();
            shell["id"] = serde_json::json!(side_target);
            shell["runtimeMode"] = serde_json::json!("full-access");
            shell["lineage"] = serde_json::json!({
                "parentThreadId": "menu-thread",
                "relationshipToParent": null,
                "rootThreadId": "menu-thread",
                "sideChat": true
            });
            state.shell.snapshot.as_mut().unwrap().threads.push(serde_json::from_value(shell).unwrap());
            state.sync_side_threads(cx);
        });

        let mut saw_runtime_mode = false;
        let mut first_msg = None;
        for _ in 0..2 {
            let (method, cmd, reply) = fixture.next_request().await;
            assert_eq!(method, t3_core::commands::method::DISPATCH_COMMAND);
            reply.send(Ok(serde_json::json!({"sequence": 2}))).unwrap();
            match cmd["type"].as_str() {
                Some("thread.runtime-mode.set") => {
                    assert_eq!(cmd["runtimeMode"], "approval-required");
                    saw_runtime_mode = true;
                }
                Some("message.dispatch") => first_msg = Some(cmd),
                other => panic!("unexpected command {other:?}"),
            }
        }
        assert!(saw_runtime_mode);
        let first_msg = first_msg.expect("first message dispatched");
        assert_eq!(first_msg["runtimeMode"], "approval-required");
        assert_eq!(first_msg["modelSelection"]["model"], "replay-custom");

        // Deleting the parent clears drafts on both the parent thread and its side chat.
        let (composer, vcx) = cx.add_window_view(|window, cx| crate::composer::Composer::new(state.clone(), window, cx));
        vcx.update(|_, cx| {
            composer.update(cx, |composer, cx| {
                composer.set_thread_draft_fixture("menu-thread", "parent draft", None, 10, cx);
                composer.set_thread_draft_fixture(&side_target, "side draft", None, 11, cx);
            });
            state.update(cx, |state, cx| {
                state.apply_batch(
                    vec![t3_client::supervisor::ClientEvent::Shell(t3_core::shell::ShellStreamItem::ThreadRemoved {
                        sequence: 3,
                        thread_id: "menu-thread".into(),
                    })],
                    cx,
                );
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let composer = composer.read(cx);
            assert!(!composer.has_thread_draft("menu-thread"));
            assert!(!composer.has_thread_draft(&side_target));
        });
        let _ = vcx;
        state.update(cx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
        drop(fixture);
        drop(runtime);
        cx.run_until_parked();
    }
}
