//! The open thread's worktree setup, shared by the transcript (the card and the working row) and
//! the composer (the send block). Ports `apps/web/src/components/chat/useWorktreeSetup.ts`: it
//! subscribes while the thread has a worktree or its run is being prepared, holds the last
//! snapshot after the server drops it, and runs Cancel and Work locally. The pure rules live in
//! `t3_core::worktree_setup`, the subscription in `t3_client::worktree_setup`.

use std::sync::Arc;

use gpui::{Action, App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};
use serde_json::Value;
use t3_core::commands::method::LAUNCH_THREAD;
use t3_core::timeline::{LatestRun, derive_activity};
use t3_core::worktree_setup::{self as rules, DraftLaunch, Feed, Snapshot};

use crate::app_state::{AppEvent, AppState, uuid_v4};
use crate::toast::{Toast, ToastKind};

/// "Open terminal": shows the setup script's terminal in the open thread's drawer.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = worktree_setup, no_json)]
pub struct OpenSetupTerminal {
    pub terminal_id: String,
}

/// What the transcript and the composer read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetupView {
    /// The card's snapshot (`resolveVisibleWorktreeSetup`).
    pub visible: Option<Arc<Snapshot>>,
    /// `isPreparingWorktree`: the working row says the worktree is being set up.
    pub preparing: bool,
    /// The composer holds the send back.
    pub blocks_send: bool,
    /// The run waits on preparation, so a send queues behind it (`sendQueuesBehindSetup`).
    pub queues_behind_setup: bool,
    /// The agent took over while the setup script still runs: the working row's chip.
    pub background: Option<Arc<Snapshot>>,
    /// "Work locally" is offered: the thread was just launched from the draft and setup runs.
    pub work_locally: bool,
}

pub enum SetupEvent {
    Changed,
}

pub struct WorktreeSetupStore {
    state: Entity<AppState>,
    thread_id: Option<String>,
    connection: Option<u64>,
    feed: Feed,
    stream: Option<(t3_client::worktree_setup::WorktreeSetupStream, Task<()>)>,
    /// The server failed the subscription; it waits for the session to be replaced.
    failed: bool,
    view: SetupView,
    /// The thread the composer just launched from its draft in a new worktree.
    draft_launch: Option<DraftLaunch>,
    /// A cancel is in flight, so a second click does nothing.
    cancelling: bool,
    /// A snapshot fixture stands in for the server's (`--worktree-setup`).
    fixture: bool,
    _subscription: Subscription,
}

impl EventEmitter<SetupEvent> for WorktreeSetupStore {}

struct Store(Entity<WorktreeSetupStore>);

impl Global for Store {}

/// The app's worktree setup store, created with the first view that reads it.
pub fn store(state: &Entity<AppState>, cx: &mut App) -> Entity<WorktreeSetupStore> {
    if let Some(store) = cx.try_global::<Store>() {
        return store.0.clone();
    }
    let store = cx.new(|cx| WorktreeSetupStore::new(state.clone(), cx));
    cx.set_global(Store(store.clone()));
    store
}

impl WorktreeSetupStore {
    fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            // A reconnect replaces the session, so a subscription the server failed may try again.
            if *event == AppEvent::Phase {
                this.failed = false;
            }
            if matches!(event, AppEvent::Selection | AppEvent::Thread | AppEvent::Shell | AppEvent::Phase) {
                this.sync(cx);
            }
        });
        let mut store = WorktreeSetupStore {
            state,
            thread_id: None,
            connection: None,
            feed: Feed::default(),
            stream: None,
            failed: false,
            view: SetupView::default(),
            draft_launch: None,
            cancelling: false,
            fixture: false,
            _subscription: subscription,
        };
        store.sync(cx);
        store
    }

    /// What `thread_id` shows; nothing while the store still follows another thread.
    pub fn view_for(&self, thread_id: Option<&str>) -> Option<&SetupView> {
        (thread_id.is_some() && self.thread_id.as_deref() == thread_id).then_some(&self.view)
    }

    /// The open thread's setup, and its subscription while the thread has a worktree or its run
    /// is being prepared (`useWorktreeSetup`'s query condition).
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let thread_id = state.selected_thread.clone();
        let connection = state.connection_id();
        if thread_id != self.thread_id {
            self.thread_id = thread_id.clone();
            self.feed = Feed::default();
            self.stream = None;
            self.failed = false;
            self.fixture = false;
        }
        if connection != self.connection {
            self.connection = connection;
            self.stream = None;
            self.failed = false;
            // The launch belongs to the environment it was sent to.
            self.draft_launch = None;
        }
        let has_worktree = thread_id.as_deref().and_then(|id| state.thread_shell(id)).is_some_and(|thread| thread.worktree_path.is_some());
        let run = self.activity_run(cx);
        let run_status = run.as_ref().map(|run| run.status.as_str());
        let wanted = !self.fixture && connection.is_some() && thread_id.is_some() && rules::wants_subscription(has_worktree, run_status, self.failed);
        match (wanted, self.stream.is_some()) {
            (true, false) => self.subscribe(cx),
            (false, true) => {
                self.stream = None;
                self.feed.apply(None);
            }
            _ => {}
        }
        self.recompute(run.as_ref(), cx);
    }

    /// The open thread's current run, as the timeline reads it (`activityRun`).
    fn activity_run(&self, cx: &App) -> Option<LatestRun> {
        let thread_id = self.thread_id.as_deref()?;
        let projection = self.state.read(cx).thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id)?;
        derive_activity(projection).latest_run
    }

    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let (Some(thread_id), Some(client)) = (self.thread_id.clone(), state.client_handle()) else { return };
        let stream = t3_client::worktree_setup::subscribe(state.runtime(), client, &thread_id, self.feed.clone());
        let mut feed = stream.feed.clone();
        let connection = self.connection;
        let pump = cx.spawn(async move |this, cx| {
            while feed.changed().await.is_ok() {
                let next = feed.borrow_and_update().clone();
                let applied = this.update(cx, |store, cx| {
                    if store.thread_id.as_deref() == Some(thread_id.as_str()) && !store.fixture {
                        store.feed = next;
                        store.refresh(cx);
                    }
                });
                if applied.is_err() {
                    return;
                }
            }
            // The subscription only ends on a server failure: let the handle go, so the next
            // session subscribes again.
            let _ = this.update(cx, |store, cx| {
                if store.thread_id.as_deref() == Some(thread_id.as_str()) && store.connection == connection && store.stream.is_some() {
                    store.stream = None;
                    store.failed = true;
                    store.feed.apply(None);
                    store.refresh(cx);
                }
            });
        });
        self.stream = Some((stream, pump));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let run = self.activity_run(cx);
        self.recompute(run.as_ref(), cx);
    }

    fn recompute(&mut self, run: Option<&LatestRun>, cx: &mut Context<Self>) {
        let next = self.derive(run, cx);
        if next != self.view {
            self.view = next;
            cx.emit(SetupEvent::Changed);
        }
    }

    fn derive(&self, run: Option<&LatestRun>, cx: &App) -> SetupView {
        let Some(thread_id) = self.thread_id.as_deref() else { return SetupView::default() };
        let projection = self.state.read(cx).thread.projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let run_status = run.map(|run| run.status.as_str());
        let turn_started = run.is_some_and(|run| run.started_at.is_some());
        let progress = rules::progress(thread_id, false, run_status, &self.feed);
        let follow_up_sent = progress.snapshot.is_some()
            && projection.is_some_and(|projection| {
                projection.messages.iter().filter(|message| message.value.get("role").and_then(Value::as_str) == Some("user")).count() > 1
            });
        let visible = rules::resolve_visible(progress.snapshot.as_ref(), None, turn_started, follow_up_sent);
        let queues_behind_setup = rules::send_queues_behind_setup(run_status);
        let blocks_send = rules::blocks_send(visible.as_deref(), progress.preparing, queues_behind_setup);
        let work_locally = visible.as_ref().is_some_and(|snapshot| snapshot.running())
            && self.draft_launch.as_ref().is_some_and(|launch| launch.thread_id == thread_id && Some(launch.connection) == self.connection);
        SetupView { background: rules::background(visible.as_ref(), turn_started), visible, preparing: progress.preparing, blocks_send, queues_behind_setup, work_locally }
    }

    /// The composer launched a thread from its draft into a new worktree; "Work locally" may
    /// resend it in the project checkout.
    pub fn record_launch(&mut self, launch: DraftLaunch, cx: &mut Context<Self>) {
        self.draft_launch = Some(launch);
        self.refresh(cx);
    }

    /// `cancel`: interrupts a running setup; the server removes the half-built worktree. Resolves
    /// to whether it was cancelled. Failures are quiet, as on the web.
    fn request_cancel(&mut self, cx: &mut Context<Self>) -> Option<impl Future<Output = bool> + use<>> {
        let snapshot = self.view.visible.clone().filter(|snapshot| snapshot.running())?;
        if self.cancelling {
            return None;
        }
        self.cancelling = true;
        let request = self.state.read(cx).rpc(rules::method::CANCEL, rules::thread_request(&snapshot.thread_id));
        Some(async move { request.await.is_ok_and(|result| rules::cancelled(&result)) })
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = self.request_cancel(cx) else { return };
        cx.spawn(async move |this, cx| {
            cancel.await;
            let _ = this.update(cx, |store, _| store.cancelling = false);
        })
        .detach();
    }

    /// `onWorktreeSetupWorkLocally`: cancels, then sends the draft's message again as a new
    /// thread in the project checkout and opens it.
    pub fn work_locally(&mut self, cx: &mut Context<Self>) {
        if !self.view.work_locally {
            return;
        }
        let Some(launch) = self.draft_launch.clone() else { return };
        let Some(cancel) = self.request_cancel(cx) else { return };
        cx.spawn(async move |this, cx| {
            let cancelled = cancel.await;
            let relaunch = this.update(cx, |store, cx| {
                store.cancelling = false;
                if !cancelled {
                    return None;
                }
                store.draft_launch = None;
                store.refresh(cx);
                // The user may have switched environments while the cancel was out.
                let thread_id = uuid_v4();
                let payload = launch.relaunch(store.state.read(cx).connection_id(), &uuid_v4(), &thread_id, &uuid_v4())?;
                Some((thread_id, store.state.read(cx).rpc(LAUNCH_THREAD, payload)))
            });
            let Ok(Some((thread_id, request))) = relaunch else { return };
            let result = request.await;
            let _ = this.update(cx, |store, cx| {
                store.state.update(cx, |state, cx| match result {
                    Ok(_) if state.connection_id() != Some(launch.connection) => {}
                    Ok(_) => state.select_when_listed(&thread_id, cx),
                    Err(error) => state.show_toast(Toast::new(ToastKind::Error, "Could not start the thread locally").description(error.to_string()), cx),
                });
            });
        })
        .detach();
    }

    /// Shows `snapshot` for the open thread instead of the server's, for offscreen captures.
    #[cfg(feature = "snapshot")]
    pub fn show_fixture(&mut self, mut snapshot: Snapshot, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id.clone() else { return };
        snapshot.thread_id = thread_id;
        self.fixture = true;
        self.stream = None;
        self.feed = Feed::default();
        self.feed.apply(Some(Arc::new(snapshot)));
        self.refresh(cx);
    }
}
