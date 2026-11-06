//! App-wide state: the connection, the shell, and the open thread. Network events arrive on a
//! channel from the client's runtime and are applied in batches, one notify per batch.

use std::collections::VecDeque;
use std::sync::Arc;
use std::future::Future;

use gpui::{Context, EventEmitter, Subscription, Task};
use serde_json::Value;
use t3_client::session::RpcError;
use t3_client::ssh::{PasswordRequest, SshEnvironments};
use t3_client::store::{self, SavedEnvironments};
use t3_client::supervisor::{ClientEvent, ClientHandle, ConnectionStatus};
use t3_core::commands::{self, method};
use t3_core::composer::ClientSettings;
use t3_core::history::{HistoryStep, PageOutcome};
use t3_core::server_config::ServerConfigState;
use t3_core::shell::{ShellState, ShellStreamItem};
use t3_core::sidebar::history::NavEntry;
use t3_core::thread::ThreadState;
use t3_core::workflows::{self, StopTarget};

use crate::environments::ConnectScreen;
use crate::sidebar::controller::SidebarController;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// No environment connected: the connect screen.
    Unpaired,
    Online(ConnectionStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    RestoreDraft,
    BranchDraft,
    PromptLink,
    /// The connection, the saved environments, or the connect screen changed. Also sent for
    /// every new connection, so whatever follows one can check `connection_id`.
    Phase,
    Shell,
    Selection,
    Thread,
    ServerConfig,
    /// Only `thread.history_control()` changed: loading, error, or more history.
    History,
    /// Toasts and the Undo notice.
    Notice,
    /// Right panel tabs, visibility, width, or the diff selection changed.
    RightPanel,
    /// A file was queued in `composer_mentions` for the composer.
    ComposerMention,
    /// The sidebar's multi-selection or thread drag changed.
    SidebarSelection,
    /// A prompt is waiting for the composer (`take_composer_restore`).
    ComposerRestore,
    /// A terminal selection is waiting for the composer (`take_terminal_context`).
    ComposerTerminalContext,
    ComposerReviewComment,
    ReviewComments,
    RemoveReviewComment,
    ComposerCitation,
    CitationSource,
    /// A side subscription (side chat tab, child approval) changed, or an action on one did.
    SideThread,
    /// An SSH password prompt arrived or was answered (`ssh_prompts`).
    SshPrompt,
}

pub struct AppState {
    pub(crate) github_star: crate::github_star_prompt::GitHubStarPrompt,
    pub(crate) announcements: crate::announcements::Announcements,
    pub phase: Phase,
    pub environments: SavedEnvironments,
    pub(crate) environment_statuses: std::collections::HashMap<String, ConnectionStatus>,
    /// `environments` as last handed to the store; saves send only the edits since.
    pub(crate) environments_saved: SavedEnvironments,
    pub connect: ConnectScreen,
    pub cloud: crate::cloud::CloudState,
    pub shell: ShellState,
    pub(crate) fleet: Arc<t3_client::environments::FleetView>,
    pub(crate) project_preferences: t3_core::sidebar::grouping::ProjectPreferences,
    pending_scoped_route: Option<ScopedRoute>,
    _fleet_pump: Option<Task<()>>,
    pub sidebar: SidebarController,
    pub(crate) open_history: t3_core::sidebar::visits::OpenHistory,
    pub prompt_link_urls: VecDeque<String>,
    pub toasts: crate::toast::Toasts,
    pub composer_mentions: Vec<crate::project_files::ComposerMention>,
    pub selected_thread: Option<String>,
    pub thread: ThreadState,
    pub last_error: Option<String>,
    pub server_config: ServerConfigState,
    pub client_settings: ClientSettings,
    /// The project of the new-thread draft, while the user composes a thread that does not exist.
    pub draft_project: Option<String>,
    pub(crate) branch_draft: Option<(crate::context_menu::ThreadMenuOwner, t3_core::workspace::DraftWorkspace)>,
    pub panels: crate::right_panel::PanelState,
    /// The pull requests page replaces the thread area while open (`/pull-requests`).
    pub pull_requests_open: bool,
    restore_thread: Option<String>,
    /// The restored thread's family is being read, so home waits before opening a draft.
    pub(crate) restoring_thread: bool,
    /// Rows the shell leaves to on-demand reads: older settled pages and thread families.
    pub(crate) omitted: crate::omitted_rows::OmittedRows,
    pub(crate) notifications: crate::notifications::NotificationCenter,
    pub(crate) side_threads: crate::side_chat::SideThreads,
    /// A thread just launched from the draft, selected once the shell lists it.
    select_when_listed: Option<String>,
    /// A prompt handed back to the composer for a thread, as "Edit from here" restores one.
    composer_restore: Option<(String, String)>,
    /// A terminal selection added to a thread's chat, as the drawer hands it over.
    composer_terminal_context: Option<(String, t3_core::composer::context::TerminalSelection)>,
    pub review_comments: std::collections::HashMap<String, Vec<t3_core::composer::review::ReviewComment>>,
    remove_review_comment: Option<(String, String)>,
    composer_review_comment: Option<(String, t3_core::composer::review::ReviewComment)>,
    pub composer_citation: Option<t3_core::assistant_citations::Citation>,
    pub citation_source: Option<t3_core::assistant_citations::Citation>,
    client: Option<ClientHandle>,
    /// Counts clients started, so state tied to one never outlives a switch to the next.
    connection: u64,
    runtime: tokio::runtime::Handle,
    /// SSH environments' tunnels, owned for the life of the app.
    pub(crate) ssh: SshEnvironments,
    /// SSH password prompts waiting for an answer, oldest first, each with its sequence number.
    pub ssh_prompts: VecDeque<(u64, PasswordRequest)>,
    pub(crate) ssh_prompt_sequence: u64,
    pub(crate) connections: t3_client::environments::Environments,
    _pump: Option<Task<()>>,
    #[cfg(feature = "snapshot")]
    snapshot_frozen: bool,
    pub(crate) notification_route: Option<t3_core::sidebar::tag_filter::ThreadKey>,
    _ssh_quit: Option<Subscription>,
}

#[derive(Clone)]
pub(crate) enum ScopedRouteAction {
    Thread(String),
    Draft(String),
    Branch(String),
    ProjectSettings(String),
}

#[derive(Clone)]
struct ScopedRoute {
    owner: t3_core::sidebar::grouping::EnvironmentOwner,
    action: ScopedRouteAction,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new(runtime: tokio::runtime::Handle, cx: &mut Context<Self>) -> Self {
        let environments = store::load_environments();
        let (prompts, mut prompt_requests) = tokio::sync::mpsc::unbounded_channel();
        cx.spawn(async move |this, cx| {
            while let Some(request) = prompt_requests.recv().await {
                if this.update(cx, |state, cx| state.queue_ssh_prompt(request, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        let settings = ClientSettings::from_json(&store::load_client_settings().unwrap_or_default());
        let (connections, mut events) = t3_client::environments::Environments::start(&runtime, settings.notification_mode != t3_core::notifications::NotificationMode::Off || settings.in_app_notifications_enabled);
        let mut fleet_view = connections.view();
        let fleet = fleet_view.borrow_and_update().clone();
        let fleet_pump = cx.spawn(async move |this, cx| {
            while fleet_view.changed().await.is_ok() {
                let view = fleet_view.borrow_and_update().clone();
                if this.update(cx, |state, cx| state.apply_fleet_view(view, cx)).is_err() { return; }
            }
        });
        let pump = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this.update(cx, |state, cx| state.apply_environment_event(event, cx)).is_err() { return; }
            }
        });
        let mut state = AppState {
            announcements: crate::announcements::Announcements::new(cx),
            github_star: crate::github_star_prompt::GitHubStarPrompt::new(cx),
            phase: Phase::Unpaired,
            environments_saved: environments.clone(),
            environments,
            environment_statuses: Default::default(),
            connect: ConnectScreen::default(),
            cloud: Default::default(),
            shell: ShellState::default(),
            fleet,
            project_preferences: store::load_project_preferences(),
            pending_scoped_route: None,
            _fleet_pump: Some(fleet_pump),
            sidebar: SidebarController::default(),
            open_history: Default::default(),
            prompt_link_urls: Default::default(),
            toasts: Default::default(),
            composer_mentions: Vec::new(),
            selected_thread: None,
            thread: ThreadState::default(),
            last_error: None,
            server_config: ServerConfigState::default(),
            client_settings: settings,
            draft_project: None,
            branch_draft: None,
            panels: Default::default(),
            pull_requests_open: false,
            restore_thread: store::load_last_thread(),
            restoring_thread: false,
            omitted: Default::default(),
            notifications: Default::default(),
            side_threads: Default::default(),
            select_when_listed: None,
            composer_restore: None,
            composer_terminal_context: None,
            review_comments: Default::default(),
            remove_review_comment: None,
            composer_review_comment: None,
            composer_citation: None,
            citation_source: None,
            client: None,
            connection: 0,
            runtime,
            ssh: SshEnvironments::new(Some(prompts)),
            ssh_prompts: VecDeque::new(),
            ssh_prompt_sequence: 0,
            connections,
            _pump: Some(pump),
            #[cfg(feature = "snapshot")]
            snapshot_frozen: false,
            notification_route: None,
            _ssh_quit: None,
        };
        // Quitting stops the tunnels this app started, by the PIDs it spawned.
        state._ssh_quit = Some(cx.on_app_quit(|state, cx| {
            state.clear_pending_notifications(cx);
            state.ssh.shutdown()
        }));
        // Setup fixtures render without a client: no discovery, subscriptions, or commands.
        if crate::chatgpt_setup::fixture_active(cx) {
            state.phase=Phase::Online(ConnectionStatus::Connected);
            state.shell.synchronized=true;
            state.server_config.config=serde_json::from_value(serde_json::json!({"environment":{"environmentId":"fixture-env","label":"Fixture Computer"},"cwd":"/fixture"})).ok();
            return state;
        }
        match state.environments.active().filter(|environment| !environment.disabled && environment.credentials().is_some()).map(|environment| environment.id.clone()) {
            Some(id) => { state.start_environment(&id, cx); state.connections.activate(Some(&id)); },
            None => state.refresh_local_server(cx),
        }
        let watched: Vec<_> = state.environments.environments.iter().filter(|e| !e.disabled && e.access_token.is_some()).map(|e| e.id.clone()).collect();
        for id in watched { state.start_environment(&id, cx); }
        state.refresh_cloud(cx);
        state
    }

    /// Fixtures hold their UI state against later network events.
    #[cfg(feature = "snapshot")]
    pub(crate) fn stop_pump(&mut self) {
        self._pump = None;
        self._fleet_pump = None;
    }

    /// Turn-item fixtures keep asset RPCs alive while holding their projected UI state.
    #[cfg(feature = "snapshot")]
    pub(crate) fn freeze_snapshot(&mut self) {
        self.snapshot_frozen = true;
    }

    /// Drops the client and everything that belonged to its environment.
    pub(crate) fn clear_connection_state(&mut self, cx: &mut Context<Self>) {
        self.reset_environment_announcements();
        if let Some(client) = &self.client { client.close_all_threads(); }
        self.client = None;
        self.shell = ShellState::default();
        let mut previous = std::mem::take(&mut self.sidebar);
        self.sidebar.collapsed = previous.collapsed;
        self.sidebar.fleet = previous.fleet;
        self.sidebar.fleet_state = previous.fleet_state;
        self.sidebar.fleet_selection = previous.fleet_selection;
        self.sidebar.fleet_anchor = previous.fleet_anchor;
        self.sidebar.owner_omitted = previous.owner_omitted;
        self.sidebar.owner_favicons = previous.owner_favicons;
        self.sidebar.owner_extra_threads = previous.owner_extra_threads;
        self.sidebar.undo = previous.undo;
        self.sidebar.undo_timer = previous.undo_timer;
        self.sidebar.grouping_projects = std::mem::take(&mut previous.grouping_projects);
        self.sidebar.grouping_threads = std::mem::take(&mut previous.grouping_threads);
        self.omitted = Default::default();
        self.selected_thread = None;
        self.select_when_listed = None;
        self.restore_thread = None;
        self.restoring_thread = false;
        self.thread = ThreadState::default();
        // Side subscriptions and a side chat start (with its timeout) belong to this environment.
        self.side_threads = Default::default();
        self.server_config = ServerConfigState::default();
        crate::theme::set_environment_themes(&[], cx);
        self.draft_project = None;
        self.branch_draft = None;
        self.pull_requests_open = false;
        self.composer_mentions.clear();
        self.composer_terminal_context = None;
        self.composer_citation = None;
        self.composer_review_comment = None;
        self.review_comments.clear();
        self.remove_review_comment = None;
        self.citation_source = None;
        self.last_error = None;
        cx.emit(AppEvent::Shell);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
        cx.emit(AppEvent::Phase);
    }

    fn apply_environment_event(&mut self, event: t3_client::environments::EnvironmentEvent, cx: &mut Context<Self>) {
        #[cfg(feature = "snapshot")]
        if self.snapshot_frozen { return; }
        use t3_client::environments::EnvironmentEvent;
        if !self.connections.is_current(&event) { return; }
        match event {
            EnvironmentEvent::Activated { id, client, shell, config, status, .. } => {
                if self.environments.active.as_deref() != Some(&id) { return; }
                self.install_active_environment(client, *shell, *config, status, cx);
            }
            EnvironmentEvent::Active { id, events, .. } => {
                if self.environments.active.as_deref() == Some(&id) { self.apply_batch(events, cx); }
            }
            EnvironmentEvent::Status { id, status, .. } => {
                self.environment_statuses.insert(id.clone(), status.clone());
                if self.environments.active.as_deref() != Some(&id) { cx.emit(AppEvent::Phase); }
                if let ConnectionStatus::NeedsPairing { reason } = status {
                    self.environment_credentials_rejected(&id, reason, cx);
                } else if self.environments.active.as_deref() == Some(&id) {
                    self.set_phase(Phase::Online(status), cx);
                }
            }
            EnvironmentEvent::Moved { id, http_base, .. } => self.environment_route_moved(&id, http_base, cx),
            EnvironmentEvent::Identity { id, descriptor, .. } => {
                if self.environments.update_identity(&id, &descriptor.environment_id, &descriptor.label, &descriptor.server_version) { self.persist_environments(); }
            }
            EnvironmentEvent::Visibility(visibility) => {
                self.notifications.visibility = visibility;
                self.refresh_grouping_cache();
                self.rebuild_sidebar(cx);
                self.retire_hidden_notifications(cx);
                cx.emit(AppEvent::Shell);
            }
            EnvironmentEvent::Notifications { id, notifications, .. } => {
                if self.environments.get(&id).is_none_or(|e| e.disabled || e.access_token.is_none()) { return; }
                for notification in notifications { self.present_notification(&id, notification, cx); }
            }
        }
    }

    pub(crate) fn install_active_environment(&mut self, client: ClientHandle, shell: ShellState, config: ServerConfigState, status: ConnectionStatus, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.connection += 1;
        self.shell = shell;
        self.server_config = config;
        self.panels = crate::right_panel::PanelState::load(self.environments.active.as_deref());
        cx.emit(AppEvent::RightPanel);
        self.set_phase(Phase::Online(status), cx);
        cx.emit(AppEvent::Phase);
        self.sync_server_config(cx);
        self.sync_shell_views(true, true, self.shell.synchronized, cx);
        self.finish_notification_route(cx);
        let scoped_navigation = self.pending_scoped_route.is_some();
        self.finish_scoped_route(cx);
        if !scoped_navigation { self.land_home(cx); }
    }

    fn apply_fleet_view(&mut self, view: Arc<t3_client::environments::FleetView>, cx: &mut Context<Self>) {
        if self.fleet.revision == view.revision { return; }
        let summaries_changed = self.fleet.environments.len() != view.environments.len() || self.fleet.environments.iter().zip(&view.environments).any(|(previous, next)| !Arc::ptr_eq(&previous.summary, &next.summary));
        let projects_changed = self.fleet.environments.len() != view.environments.len() || self.fleet.environments.iter().zip(&view.environments).any(|(previous, next)| !Arc::ptr_eq(&previous.summary, &next.summary) && previous.summary.projects != next.summary.projects);
        let previous = projects_changed.then(|| self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx));
        self.fleet = view;
        if summaries_changed { self.refresh_grouping_cache(); }
        if let Some(previous) = previous {
            let next = self.project_groups_for(t3_core::sidebar::grouping::GroupConsumer::Sidebar, cx);
            let before = self.project_preferences.clone();
            self.project_preferences.reconcile(&previous, &next);
            if self.project_preferences != before { self.save_project_preferences(cx); }
        }
        self.sync_scoped_omitted_rows(cx);
        self.ensure_scoped_favicons(cx);
        self.rebuild_sidebar(cx);
        self.finish_scoped_route(cx);
        cx.emit(AppEvent::Shell);
        cx.emit(AppEvent::ServerConfig);
    }

    fn refresh_grouping_cache(&mut self) {
        self.sidebar.grouping_projects = self.fleet.environments.iter().flat_map(|entry| entry.summary.projects.iter().cloned()).collect();
        self.sidebar.grouping_threads = self.fleet.environments.iter().flat_map(|entry| entry.summary.threads.iter()).filter(|thread| self.notifications.visibility.visible(&thread.key())).cloned().collect();
    }

    pub(crate) fn active_thread_key(&self, id: &str) -> Option<t3_core::sidebar::tag_filter::ThreadKey> {
        Some(t3_core::sidebar::tag_filter::ThreadKey::new(self.environments.active.as_deref()?, id))
    }

    pub(crate) fn scoped_environment(&self, saved: &str) -> Option<&t3_client::environments::EnvironmentView> {
        self.fleet.environments.iter().find(|entry| entry.summary.owner.saved_id == saved).map(AsRef::as_ref)
    }

    pub(crate) fn scoped_thread(&self, key: &t3_core::sidebar::tag_filter::ThreadKey) -> Option<&t3_core::sidebar::fleet::ScopedThread> {
        let summary = &self.scoped_environment(&key.environment_id)?.summary;
        summary.threads.iter().chain(self.sidebar.owner_extra_threads.get(&summary.owner).into_iter().flatten()).find(|thread| thread.thread.id == key.thread_id)
    }

    pub(crate) fn scoped_project(&self, key: &t3_core::sidebar::grouping::ProjectRef) -> Option<&t3_core::sidebar::grouping::ScopedProject> {
        self.scoped_environment(&key.environment_id)?.summary.projects.iter().find(|project| project.project.id == key.project_id)
    }

    pub(crate) fn scoped_ready(&self, owner: &t3_core::sidebar::grouping::EnvironmentOwner, scope: &str) -> bool {
        self.scoped_environment(&owner.saved_id).is_some_and(|entry| {
            entry.summary.owner == *owner && entry.summary.synchronized
                && entry.status == ConnectionStatus::Connected
                && entry.grants.as_ref().is_some_and(|grants| grants.grants(scope))
        })
    }

    pub(crate) fn navigate_scoped(&mut self, owner: t3_core::sidebar::grouping::EnvironmentOwner, action: ScopedRouteAction, cx: &mut Context<Self>) {
        if !self.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) { return; }
        if let ScopedRouteAction::Thread(id)=&action && self.scoped_thread(&t3_core::sidebar::tag_filter::ThreadKey::new(&owner.saved_id,id)).is_some_and(|_| self.scoped_environment(&owner.saved_id).is_some_and(|entry| !entry.summary.threads.iter().any(|row| row.thread.id==*id))) {
            self.read_scoped_family(&owner,id,cx);
        }
        self.pending_scoped_route = Some(ScopedRoute { owner: owner.clone(), action });
        if self.environments.active.as_deref() == Some(&owner.saved_id) {
            self.finish_scoped_route(cx);
            return;
        }
        // The destination is already watched: navigation must not discover, pair or start it.
        self.environments.active = Some(owner.saved_id.clone());
        self.persist_environments();
        self.clear_connection_state(cx);
        self.connections.activate(Some(&owner.saved_id));
    }

    fn finish_scoped_route(&mut self, cx: &mut Context<Self>) {
        let Some(route) = self.pending_scoped_route.clone() else { return; };
        if !self.scoped_ready(&route.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
            self.pending_scoped_route = None;
            return;
        }
        if self.environments.active.as_deref() != Some(&route.owner.saved_id)
            || !self.shell.synchronized || self.shell.generation != route.owner.shell_generation
            || self.client_handle().is_none_or(|client| *client.connection_generation().borrow() != route.owner.connection_generation)
        { return; }
        self.pending_scoped_route = None;
        match route.action {
            ScopedRouteAction::Thread(id) => {
                if self.thread_shell(&id).is_none() { self.read_family(&id,true,cx); }
                else { self.select_thread(&id,cx); }
            },
            ScopedRouteAction::Draft(id) => self.start_draft(&id, cx),
            ScopedRouteAction::Branch(id) => self.start_branch_draft(&id, cx),
            ScopedRouteAction::ProjectSettings(id) => crate::settings::open_project(&id, cx),
        }
    }

    pub(crate) fn apply_batch(&mut self, batch: Vec<ClientEvent>, cx: &mut Context<Self>) {
        let (mut shell_changed, mut sidebar_changed, mut thread_changed, mut config_changed) = (false, false, false, false);
        // A resubscribed shell is live again at its completion marker, even with nothing new.
        let mut resynchronized = false;
        let mut side_changed = false;
        for event in batch {
            match event {
                ClientEvent::Status(ConnectionStatus::NeedsPairing { reason }) => { if let Some(id) = self.environments.active.clone() { self.environment_credentials_rejected(&id, reason, cx); } },
                ClientEvent::Status(status) => self.set_phase(Phase::Online(status), cx),
                ClientEvent::Shell(item) => {
                    resynchronized |= matches!(item, ShellStreamItem::Synchronized);
                    let quiet = crate::sidebar::controller::is_streaming_tick(&self.shell, &item);
                    if self.shell.apply(item) {
                        shell_changed = true;
                        sidebar_changed |= !quiet;
                    }
                }
                ClientEvent::Thread { thread_id, item } => {
                    if self.selected_thread.as_deref() == Some(thread_id.as_str()) {
                        thread_changed |= self.thread.apply(*item);
                    } else {
                        side_changed |= self.apply_side_thread(&thread_id, *item);
                    }
                }
                ClientEvent::ServerConfig(event) => config_changed |= self.server_config.apply(*event),
                ClientEvent::StreamError { context, message } => {
                    self.last_error = Some(format!("Could not read {context}: {message}"));
                }
                ClientEvent::Moved { http_base } => { if let Some(id) = self.environments.active.clone() { self.environment_route_moved(&id, http_base, cx); } },
            }
        }
        if side_changed {
            cx.emit(AppEvent::SideThread);
        }
        self.sync_shell_views(shell_changed, sidebar_changed, resynchronized, cx);
        if thread_changed && self.thread.deleted {
            // The web treats a deleted open thread as missing and goes home.
            if !self.hold_pending_thread_deletion() {
                self.close_selected_thread(cx);
            }
        } else if thread_changed {
            cx.emit(AppEvent::Thread);
        }
        if shell_changed || resynchronized {
            self.land_home(cx);
        }
        if config_changed {
            self.sync_server_config(cx);
            self.sync_environment_identity();
            cx.emit(AppEvent::ServerConfig);
        }
        if shell_changed || resynchronized {
            self.finish_notification_route(cx);
        }
    }

    fn sync_shell_views(&mut self, shell_changed: bool, sidebar_changed: bool, resynchronized: bool, cx: &mut Context<Self>) {
        if sidebar_changed || resynchronized { self.observe_background_announcements(cx); }
        if shell_changed {
            self.sync_omitted_rows(cx);
            self.sync_side_threads(cx);
            if sidebar_changed {
                self.rebuild_sidebar(cx);
            } else {
                self.track_visit(cx);
            }
            cx.emit(AppEvent::Shell);
            self.restore_shell_selection(cx);
        }
    }

    fn restore_shell_selection(&mut self, cx: &mut Context<Self>) {
        let (shell, mounts) = (&self.shell, &self.omitted.mounts);
        if let Some(id) = self.select_when_listed.take_if(|id| mounts.thread(shell, id).is_some()) {
            self.sidebar.history.replace_draft();
            self.select_thread(&id, cx);
        } else if self.selected_thread.is_none()
            && self.draft_project.is_none()
            && let Some(id) = self.restore_thread.take_if(|id| mounts.thread(shell, id).is_some())
        {
            self.select_thread(&id, cx);
        } else if self.selected_thread.is_none()
            && self.draft_project.is_none()
            && self.shell.synchronized
            && let Some(id) = self.restore_thread.take()
        {
            // A shell that omits older settled or idle threads can still hold it: ask its family.
            self.read_family(&id, true, cx);
        }
    }

    /// With nothing open, the home page opens a draft in the most recent project, as the web's
    /// index route does (`_chat.index.tsx`). It waits while a thread is about to be selected.
    pub(crate) fn land_home(&mut self, cx: &mut Context<Self>) {
        let idle = self.selected_thread.is_none() && self.draft_project.is_none() && !self.pull_requests_open;
        let waiting = self.notification_route.is_some() || self.restore_thread.is_some() || self.select_when_listed.is_some() || self.restoring_thread;
        if !idle || waiting || !self.shell.synchronized {
            return;
        }
        if let Some(project_id) = self.home_project() {
            self.start_draft(&project_id, cx);
        }
    }

    pub(crate) fn close_selected_thread(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(thread_id)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&thread_id);
        }
        self.track_visit(cx);
        self.release_families(None);
        self.thread = ThreadState::default();
        self.sync_side_threads(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
        self.land_home(cx);
    }

    pub(crate) fn set_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            self.sync_notification_state(cx);
            cx.emit(AppEvent::Phase);
            // Connection state places threads on the Working shelf.
            if self.shell.snapshot.is_some() {
                self.rebuild_sidebar(cx);
                cx.emit(AppEvent::Shell);
            }
        }
    }

    pub fn select_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.notification_route = None;
        if !self.thread_route_visible(thread_id) {
            self.restore_thread = None;
            self.clear_selection(cx);
            return;
        }
        self.retire_thread_notifications(thread_id, cx);
        if self.selected_thread.as_deref() == Some(thread_id) {
            return self.set_pull_requests_open(false, cx);
        }
        self.pull_requests_open = false;
        self.draft_project = None;
        self.branch_draft = None;
        if let (Some(client), Some(previous)) = (&self.client, &self.selected_thread) {
            client.close_thread(previous);
        }
        self.selected_thread = Some(thread_id.to_owned());
        self.track_visit(cx);
        self.thread = ThreadState::default();
        self.adopt_side_thread(thread_id);
        self.restore_thread = None;
        store::save_last_thread(thread_id);
        self.sidebar.history.visit(NavEntry::Thread(thread_id.to_owned()));
        if let Some(client) = &self.client {
            client.open_thread(thread_id);
        }
        // An idle subagent or older settled thread opens from its family when the shell omits it.
        self.release_families(Some(thread_id));
        self.mount_selected_family(cx);
        self.sync_side_threads(cx);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    /// Loads the page of history before the open thread's window, and keeps paging while pages
    /// hold no user turn (`threads.ts` `loadEarlier`). The transcript's "Load earlier turns"
    /// control and its retry call this. A no-op while loading or at the start.
    pub fn load_older_history(&mut self, cx: &mut Context<Self>) {
        let (Some(thread_id), Some(client)) = (self.selected_thread.clone(), self.client.clone()) else { return };
        let Some(cursor) = self.thread.begin_history_load() else { return };
        cx.emit(AppEvent::History);
        self.fetch_history_page(thread_id, cursor, client, cx);
    }

    /// Fetches and decodes on the network runtime, merges on the background executor, and
    /// touches the UI thread only to check the projection out and swap the result back.
    fn fetch_history_page(&mut self, thread_id: String, cursor: String, client: ClientHandle, cx: &mut Context<Self>) {
        let task = {
            let (client, thread_id, cursor) = (client.clone(), thread_id.clone(), cursor.clone());
            self.runtime.spawn(async move { client.history_page(&thread_id, &cursor).await })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
            let started = this.update(cx, |state, cx| {
                if state.selected_thread.as_deref() != Some(thread_id.as_str()) {
                    return None;
                }
                let outcome = match result {
                    Ok(page) => state.thread.begin_history_merge(&cursor, page),
                    Err(message) => PageOutcome::Step(state.thread.fail_history_load(&cursor, message)),
                };
                match outcome {
                    PageOutcome::Merge(merge) => {
                        cx.emit(AppEvent::History);
                        Some(merge)
                    }
                    PageOutcome::Step(step) => {
                        if step != HistoryStep::Stale {
                            cx.emit(AppEvent::History);
                        }
                        None
                    }
                }
            });
            let Ok(Some(merge)) = started else { return };
            let merged = cx.background_executor().spawn(async move { merge.run() }).await;
            let _ = this.update(cx, |state, cx| {
                if state.selected_thread.as_deref() != Some(thread_id.as_str()) {
                    return;
                }
                let step = state.thread.finish_history_merge(merged);
                if step == HistoryStep::Stale {
                    return;
                }
                cx.emit(AppEvent::Thread);
                cx.emit(AppEvent::History);
                if let HistoryStep::Continue(next) = step {
                    state.fetch_history_page(thread_id, next, client, cx);
                }
            });
        })
        .detach();
    }

    /// Opens the new-thread draft for `project_id`, leaving the open thread.
    pub fn start_draft(&mut self, project_id: &str, cx: &mut Context<Self>) {
        self.notification_route = None;
        if self.draft_project.as_deref() == Some(project_id) && self.selected_thread.is_none() {
            return self.set_pull_requests_open(false, cx);
        }
        self.pull_requests_open = false;
        if let (Some(client), Some(previous)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&previous);
        }
        self.thread = ThreadState::default();
        self.restore_thread = None;
        self.draft_project = Some(project_id.to_owned());
        self.sidebar.history.visit(NavEntry::Draft(project_id.to_owned()));
        self.sync_side_threads(cx);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.notification_route = None;
        if let (Some(client), Some(previous)) = (&self.client, self.selected_thread.take()) {
            client.close_thread(&previous);
        }
        self.thread = ThreadState::default();
        self.sync_side_threads(cx);
        self.rebuild_sidebar(cx);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
        self.land_home(cx);
    }

    /// Selects a thread the draft launched as soon as the shell lists it.
    pub fn select_when_listed(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.thread_shell(thread_id).is_some() {
            self.sidebar.history.replace_draft();
            self.select_thread(thread_id, cx);
        } else {
            self.select_when_listed = Some(thread_id.to_owned());
        }
    }

    /// Any RPC on the network runtime. The future resolves wherever it is awaited.
    /// Nothing is sent until the future is first polled, so futures built together still run in
    /// the order they are awaited.
    pub fn rpc(&self, method: &'static str, payload: Value) -> impl Future<Output = Result<Value, RpcError>> + use<> {
        let (client, runtime) = (self.client.clone(), self.runtime.clone());
        async move {
            let Some(client) = client else { return Err(RpcError::Closed) };
            runtime.spawn(async move { client.request(method, payload).await }).await.unwrap_or(Err(RpcError::Closed))
        }
    }

    /// Dispatches commands in order, each after the previous one committed, stopping at the
    /// first failure with its index.
    pub fn dispatch_in_order(&self, commands: Vec<Value>) -> impl Future<Output = Result<(), (usize, RpcError)>> + use<> {
        let (client, runtime) = (self.client.clone(), self.runtime.clone());
        async move {
            let Some(client) = client else { return Err((0, RpcError::Closed)) };
            runtime.spawn(async move { client.dispatch_in_order(commands).await }).await.unwrap_or(Err((0, RpcError::Closed)))
        }
    }

    /// Hands `prompt` to the composer of `thread_id`, which joins it to its draft and focuses.
    pub fn restore_composer_prompt(&mut self, thread_id: &str, prompt: String, cx: &mut Context<Self>) {
        self.composer_restore = Some((thread_id.to_owned(), prompt));
        cx.emit(AppEvent::ComposerRestore);
    }

    pub fn take_composer_restore(&mut self, thread_id: &str) -> Option<String> {
        if self.composer_restore.as_ref().is_some_and(|(thread, _)| thread == thread_id) { self.composer_restore.take().map(|(_, prompt)| prompt) } else { None }
    }

    pub fn set_review_comments(&mut self, thread: &str, comments: Vec<t3_core::composer::review::ReviewComment>, cx: &mut Context<Self>) {
        if self.review_comments.get(thread).is_some_and(|held| *held == comments) { return; }
        self.review_comments.insert(thread.into(), comments);
        cx.emit(AppEvent::ReviewComments);
    }

    pub fn remove_review_comment(&mut self, thread: &str, context_id: String, cx: &mut Context<Self>) {
        self.remove_review_comment = Some((thread.into(), context_id));
        cx.emit(AppEvent::RemoveReviewComment);
    }

    pub fn take_removed_review_comment(&mut self, thread: &str) -> Option<String> {
        if self.remove_review_comment.as_ref().is_some_and(|(id, _)| id == thread) { self.remove_review_comment.take().map(|(_, id)| id) } else { None }
    }

    pub fn add_review_comment(&mut self, thread: &str, comment: t3_core::composer::review::ReviewComment, cx: &mut Context<Self>) {
        self.composer_review_comment = Some((thread.to_owned(), comment));
        cx.emit(AppEvent::ComposerReviewComment);
    }

    pub fn take_review_comment(&mut self, thread: &str) -> Option<t3_core::composer::review::ReviewComment> {
        if self.composer_review_comment.as_ref().is_some_and(|(id, _)| id == thread) {
            self.composer_review_comment.take().map(|(_, comment)| comment)
        } else { None }
    }

    /// Hands a terminal selection to the composer of `thread_id` ("Add to chat").
    pub fn add_terminal_context(&mut self, thread_id: &str, selection: t3_core::composer::context::TerminalSelection, cx: &mut Context<Self>) {
        self.composer_terminal_context = Some((thread_id.to_owned(), selection));
        cx.emit(AppEvent::ComposerTerminalContext);
    }

    pub fn take_terminal_context(&mut self, thread_id: &str) -> Option<t3_core::composer::context::TerminalSelection> {
        match &self.composer_terminal_context {
            Some((thread, _)) if thread == thread_id => self.composer_terminal_context.take().map(|(_, selection)| selection),
            _ => None,
        }
    }

    pub(crate) fn client_handle(&self) -> Option<ClientHandle> {
        self.client.clone()
    }

    /// Which client is current, `None` without one. Anything that holds a `ClientHandle` past
    /// one call compares this on `AppEvent::Phase` and lets go when it changes.
    pub(crate) fn connection_id(&self) -> Option<u64> {
        self.client.is_some().then_some(self.connection)
    }

    pub(crate) fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    pub fn is_selected_thread_running(&self) -> bool {
        self.selected_thread
            .as_deref()
            .and_then(|id| self.thread_shell(id))
            .is_some_and(|thread| thread.is_live())
    }

    /// `interruptThreadTurn`: `thread.stop` where the server has it, else interrupt the live run
    /// or the settled run that left background work running.
    pub fn stop_selected_thread(&mut self, cx: &mut Context<Self>) {
        let Some(thread_id) = self.selected_thread.clone() else { return };
        self.stop_thread(&thread_id, cx);
    }

    /// `stop_selected_thread` for any thread, such as a side chat.
    pub fn stop_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if let Some(command) = self.stop_command(thread_id) {
            self.dispatch(command, cx);
        }
    }

    /// What `interruptThreadTurn` sends to stop `thread_id`; a thread whose detail is not loaded
    /// here falls back to its shell's active run on servers without `thread.stop`.
    pub(crate) fn stop_command(&self, thread_id: &str) -> Option<Value> {
        let thread_stop = self.server_config.config.as_ref().is_none_or(|c| c.environment.capabilities.thread_stop == Some(true));
        let projection = self.thread_state(thread_id).projection.as_ref().filter(|projection| projection.thread.id == thread_id);
        let active_run = self.thread_shell(thread_id).and_then(|thread| thread.active_run_id.as_deref());
        match workflows::stop_target(projection, active_run, thread_stop) {
            StopTarget::Thread => Some(commands::stop_thread(&uuid_v4(), thread_id)),
            StopTarget::Run(run_id) => Some(commands::interrupt_run(&uuid_v4(), thread_id, &run_id)),
            StopTarget::Nothing => None,
        }
    }

    /// One unary RPC, run and decoded on the network runtime so large replies never parse on the
    /// UI thread. `None` while there is no connection.
    pub fn request<T: Send + 'static>(
        &self,
        method: &'static str,
        payload: Value,
        decode: impl FnOnce(Value) -> Result<T, String> + Send + 'static,
    ) -> Option<tokio::task::JoinHandle<Result<T, String>>> {
        let client = self.client.clone()?;
        Some(self.runtime.spawn(async move { decode(client.request(method, payload).await.map_err(|error| error.to_string())?) }))
    }

    /// The connected server's address, `None` while unpaired.
    pub fn environment(&self) -> Option<&str> {
        self.client.as_ref().map(ClientHandle::environment)
    }

    fn dispatch(&mut self, command: Value, cx: &mut Context<Self>) {
        let task = self.rpc(method::DISPATCH_COMMAND, command);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |state, cx| {
                    state.last_error = Some(error.to_string());
                    cx.emit(AppEvent::Thread);
                });
            }
        })
        .detach();
    }
}

pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}
