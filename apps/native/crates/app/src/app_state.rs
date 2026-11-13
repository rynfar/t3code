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
    /// The narrow-window sheet opened or closed, or the window crossed the sheet breakpoint.
    SidebarVisibility,
    /// A prompt is waiting for the composer (`take_composer_restore`).
    ComposerRestore,
    /// A terminal selection is waiting for the composer (`take_terminal_context`).
    ComposerTerminalContext,
    ComposerReviewComment,
    ReviewComments,
    RemoveReviewComment,
    ComposerCitation,
    CitationSource,
    /// Dropped files waiting for a thread route's composer (`take_pending_composer_file_drop`).
    ComposerFileDrop,
    /// A side subscription (side chat tab, child approval) changed, or an action on one did.
    SideThread,
    /// A pending find-in-thread handoff arrived from sidebar search (`take_pending_thread_find`).
    ThreadFind,
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
    /// The left sidebar's preferred width; views clamp it to the window (`sidebar::layout`).
    pub sidebar_width: f32,
    pub sidebar_sheet: t3_core::sidebar::layout::SidebarSheet,
    pub(crate) open_history: t3_core::sidebar::visits::OpenHistory,
    pub prompt_link_urls: VecDeque<String>,
    pub toasts: crate::toast::Toasts,
    pub composer_mentions: Vec<crate::project_files::ComposerMention>,
    pub selected_thread: Option<String>,
    pub thread: ThreadState,
    /// Serializes projection checkout/merge across `fetch_history_page`, `load_earlier_through`,
    /// and parallel `load_deferred_work` tasks so sibling anchors do not collide on `projection.take()`.
    pub(crate) history_merge_lock: Arc<futures::lock::Mutex<()>>,
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
    pending_thread_find: Option<(t3_core::sidebar::tag_filter::ThreadKey, t3_core::timeline::PendingThreadFind)>,
    pending_composer_file_drop: Option<(t3_core::sidebar::tag_filter::ThreadKey, Vec<std::path::PathBuf>)>,
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
    Project { group: Box<t3_core::sidebar::grouping::ScopedProjectGroup>, action: t3_core::thread_actions::ProjectAction },
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
            sidebar_width: t3_core::sidebar::layout::preferred_width(store::load_ui_state(t3_core::sidebar::layout::STORAGE_KEY).and_then(|value| value.as_f64()).map(|width| width as f32)),
            sidebar_sheet: Default::default(),
            open_history: Default::default(),
            prompt_link_urls: Default::default(),
            toasts: Default::default(),
            composer_mentions: Vec::new(),
            selected_thread: None,
            thread: ThreadState::default(),
            history_merge_lock: Arc::new(futures::lock::Mutex::new(())),
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
            pending_thread_find: None,
            pending_composer_file_drop: None,
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
        self.sidebar.shelves = previous.shelves;
        self.sidebar.settled_scope_keys = previous.settled_scope_keys;
        self.sidebar.fleet = previous.fleet;
        self.sidebar.fleet_state = previous.fleet_state;
        self.sidebar.fleet_selection = previous.fleet_selection;
        self.sidebar.fleet_anchor = previous.fleet_anchor;
        self.sidebar.route_observed = previous.route_observed;
        self.sidebar.route_pending = previous.route_pending;
        self.sidebar.search_input = previous.search_input;
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
        let keeping_scoped_drop = matches!(
            (&self.pending_scoped_route, &self.pending_composer_file_drop),
            (Some(ScopedRoute { owner, action: ScopedRouteAction::Thread(route_thread) }), Some((drop_key, _)))
                if drop_key.environment_id == owner.saved_id && drop_key.thread_id == *route_thread
        );
        if !keeping_scoped_drop {
            self.pending_composer_file_drop = None;
        }
        self.last_error = None;
        cx.emit(AppEvent::Shell);
        cx.emit(AppEvent::Selection);
        cx.emit(AppEvent::Thread);
        cx.emit(AppEvent::Phase);
    }

    pub(crate) fn apply_environment_event(&mut self, event: t3_client::environments::EnvironmentEvent, cx: &mut Context<Self>) {
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

    pub(crate) fn apply_fleet_view(&mut self, view: Arc<t3_client::environments::FleetView>, cx: &mut Context<Self>) {
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
        self.sidebar.owner_extra_threads.get(&summary.owner).into_iter().flatten().chain(&summary.threads).find(|thread| thread.thread.id == key.thread_id)
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
        if let ScopedRouteAction::Thread(id) = &action { self.observe_scoped_route(t3_core::sidebar::tag_filter::ThreadKey::new(&owner.saved_id, id)); }
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
        if matches!(&route.action, ScopedRouteAction::Project { .. }) && self.client_handle().is_none() { return; }
        if !self.scoped_ready(&route.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
            self.pending_scoped_route = None;
            self.pending_composer_file_drop = None;
            if let ScopedRouteAction::Project { group, action } = route.action {
                self.finish_scoped_project_action(&group, action, cx);
            }
            return;
        }
        if self.environments.active.as_deref() != Some(&route.owner.saved_id)
            || !self.shell.synchronized || self.shell.generation != route.owner.shell_generation
            || self.client_handle().is_none_or(|client| *client.connection_generation().borrow() != route.owner.connection_generation)
        { return; }
        self.pending_scoped_route = None;
        match route.action {
            ScopedRouteAction::Thread(id) => {
                if self.thread_shell(&id).is_none() { self.read_family(&id, crate::omitted_rows::FamilySelect::Always, cx); }
                else { self.select_thread(&id,cx); }
            },
            ScopedRouteAction::Draft(id) => self.start_draft(&id, cx),
            ScopedRouteAction::Branch(id) => self.start_branch_draft(&id, cx),
            ScopedRouteAction::ProjectSettings(id) => crate::settings::open_project(&id, cx),
            ScopedRouteAction::Project { group, action } => self.finish_scoped_project_action(&group, action, cx),
        }
    }

    pub(crate) fn apply_batch(&mut self, batch: Vec<ClientEvent>, cx: &mut Context<Self>) {
        let (mut shell_changed, mut sidebar_changed, mut thread_changed, mut error_changed, mut config_changed) = (false, false, false, false, false);
        // A resubscribed shell is live again at its completion marker, even with nothing new.
        let mut resynchronized = false;
        let mut side_changed = false;
        for event in batch {
            match event {
                ClientEvent::Status(ConnectionStatus::NeedsPairing { reason }) => { if let Some(id) = self.environments.active.clone() { self.environment_credentials_rejected(&id, reason, cx); } },
                ClientEvent::Status(status) => self.set_phase(Phase::Online(status), cx),
                ClientEvent::Shell(item) => {
                    error_changed |= self.clear_stream_error("shell");
                    resynchronized |= matches!(item, ShellStreamItem::Synchronized);
                    let quiet = crate::sidebar::controller::is_streaming_tick(&self.shell, &item);
                    if self.shell.apply(item) {
                        shell_changed = true;
                        sidebar_changed |= !quiet;
                    }
                }
                ClientEvent::Thread { thread_id, item } => {
                    error_changed |= self.clear_stream_error(&format!("thread {thread_id}"));
                    if self.selected_thread.as_deref() == Some(thread_id.as_str()) {
                        thread_changed |= self.thread.apply(*item);
                    } else {
                        side_changed |= self.apply_side_thread(&thread_id, *item);
                    }
                }
                ClientEvent::ServerConfig(event) => {
                    error_changed |= self.clear_stream_error("server config");
                    config_changed |= self.server_config.apply(*event);
                }
                ClientEvent::StreamError { context, message } => {
                    self.last_error = Some(format!("Could not read {context}: {message}"));
                    error_changed = true;
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
        } else if thread_changed || error_changed {
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
            self.read_family(&id, crate::omitted_rows::FamilySelect::IfIdle, cx);
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
        self.pending_composer_file_drop = None;
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
        if self.pending_thread_find.as_ref().is_some_and(|(key, _)| {
            key.thread_id != thread_id
                || (!key.environment_id.is_empty() && self.environments.active.as_deref() != Some(key.environment_id.as_str()))
        }) {
            self.pending_thread_find = None;
        }
        if self.pending_composer_file_drop.as_ref().is_some_and(|(key, _)| {
            !pending_drop_matches(key, self.environments.active.as_deref(), thread_id)
        }) {
            self.pending_composer_file_drop = None;
        }
        if !self.thread_route_visible(thread_id) {
            self.pending_composer_file_drop = None;
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
        let merge_lock = self.history_merge_lock.clone();
        let task = {
            let (client, thread_id, cursor) = (client.clone(), thread_id.clone(), cursor.clone());
            self.runtime.spawn(async move { client.history_page(&thread_id, &cursor).await })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| Err("Could not load earlier activity.".into()));
            let _merge_guard = merge_lock.lock().await;
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
        self.pending_thread_find = None;
        self.pending_composer_file_drop = None;
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
        self.pending_thread_find = None;
        self.pending_composer_file_drop = None;
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

    /// Queues a find-in-thread search opened from sidebar search once `key`'s thread is shown.
    pub(crate) fn queue_thread_find(
        &mut self,
        key: t3_core::sidebar::tag_filter::ThreadKey,
        find: t3_core::timeline::PendingThreadFind,
        cx: &mut Context<Self>,
    ) {
        self.pending_thread_find = Some((key, find));
        cx.emit(AppEvent::ThreadFind);
    }

    pub(crate) fn take_pending_thread_find(&mut self, thread_id: &str) -> Option<t3_core::timeline::PendingThreadFind> {
        let active_env = self.environments.active.as_deref().unwrap_or("");
        let matches = self.pending_thread_find.as_ref().is_some_and(|(key, _)| {
            key.thread_id == thread_id && (key.environment_id.is_empty() || key.environment_id == active_env)
        });
        if matches { self.pending_thread_find.take().map(|(_, find)| find) } else { None }
    }

    pub(crate) fn drop_files_on_sidebar_thread(
        &mut self,
        key: t3_core::sidebar::tag_filter::ThreadKey,
        paths: Vec<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        if key.environment_id.is_empty() || self.fleet.environments.is_empty() {
            if self.sidebar.selection.clear() {
                cx.emit(AppEvent::SidebarSelection);
            }
            self.anchor_opened_thread(&key.thread_id, cx);
            self.pending_composer_file_drop = Some((key.clone(), paths));
            if self.thread_route_visible(&key.thread_id) {
                self.select_thread(&key.thread_id, cx);
            } else if self.server_config.config.as_ref().is_some_and(|config| config.shell_thread_families) {
                self.read_family(&key.thread_id, crate::omitted_rows::FamilySelect::Always, cx);
            } else {
                self.pending_composer_file_drop = None;
            }
            if self.pending_composer_file_drop.is_some() {
                cx.emit(AppEvent::ComposerFileDrop);
            }
            return;
        }
        if !self.sidebar.fleet_selection.is_empty() {
            self.sidebar.fleet_selection.clear();
            cx.emit(AppEvent::SidebarSelection);
        }
        self.sidebar.fleet_anchor = Some(key.clone());
        let Some(owner) = self.scoped_environment(&key.environment_id).map(|entry| entry.summary.owner.clone()) else {
            self.pending_composer_file_drop = None;
            return;
        };
        if !self.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
            self.pending_composer_file_drop = None;
            return;
        }
        self.pending_composer_file_drop = Some((key.clone(), paths));
        self.navigate_scoped(owner, ScopedRouteAction::Thread(key.thread_id), cx);
        if self.pending_composer_file_drop.is_some() {
            cx.emit(AppEvent::ComposerFileDrop);
        }
    }

    pub(crate) fn take_pending_composer_file_drop(&mut self, thread_id: &str) -> Option<Vec<std::path::PathBuf>> {
        let active_env = self.environments.active.as_deref();
        if self.pending_composer_file_drop.as_ref().is_some_and(|(key, _)| pending_drop_matches(key, active_env, thread_id)) {
            self.pending_composer_file_drop.take().map(|(_, paths)| paths)
        } else {
            None
        }
    }

    pub(crate) fn clear_pending_composer_file_drop_for(&mut self, environment_id: Option<&str>, thread_id: &str) {
        if self.pending_composer_file_drop.as_ref().is_some_and(|(key, _)| pending_drop_matches(key, environment_id, thread_id)) {
            self.pending_composer_file_drop = None;
        }
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn has_pending_composer_file_drop(&self) -> bool {
        self.pending_composer_file_drop.is_some()
    }

    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn set_pending_composer_file_drop_for_test(
        &mut self,
        key: t3_core::sidebar::tag_filter::ThreadKey,
        paths: Vec<std::path::PathBuf>,
    ) {
        self.pending_composer_file_drop = Some((key, paths));
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

    fn clear_stream_error(&mut self, context: &str) -> bool {
        let prefix = format!("Could not read {context}: ");
        if self.last_error.as_deref().is_some_and(|err| err.starts_with(&prefix)) {
            self.last_error = None;
            true
        } else {
            false
        }
    }

    /// `stop_selected_thread` for any thread, such as a side chat. Awaits the stop dispatch (if
    /// any) and then runs `reconcilePendingWork` (`threads.ts`) to settle any pending items the
    /// server already ended outside the bounded window.
    pub fn stop_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let stop_rpc = self.stop_command(thread_id).map(|command| self.rpc(method::DISPATCH_COMMAND, command));
        let thread_id = thread_id.to_owned();
        let connection = self.connection;
        cx.spawn(async move |this, cx| {
            if let Some(task) = stop_rpc
                && let Err(error) = task.await
            {
                let _ = this.update(cx, |state, cx| {
                    state.last_error = Some(error.to_string());
                    cx.emit(AppEvent::Thread);
                });
            }
            let Some((client, runtime, pending_ids)) = this
                .update(cx, |state, _| {
                    if state.connection != connection {
                        return None;
                    }
                    let client = state.client.clone()?;
                    let thread = state.thread_state(&thread_id);
                    if thread.deleted {
                        return None;
                    }
                    let projection = thread.projection.as_ref()?;
                    let ids = t3_core::thread::pending_background_turn_item_ids(projection);
                    if ids.is_empty() {
                        return None;
                    }
                    Some((client, state.runtime.clone(), ids))
                })
                .ok()
                .flatten()
            else {
                return;
            };

            let fetch_thread = thread_id.clone();
            let settled = runtime
                .spawn(async move {
                    let permits = Arc::new(tokio::sync::Semaphore::new(4));
                    let fetches = pending_ids.into_iter().map(|item_id| {
                        let permits = permits.clone();
                        let client = client.clone();
                        let thread_id = fetch_thread.clone();
                        async move {
                            let Ok(_permit) = permits.acquire().await else { return None };
                            let request = client.request(
                                t3_core::timeline::GET_TURN_ITEM,
                                serde_json::json!({ "threadId": thread_id, "itemId": item_id }),
                            );
                            let value = tokio::time::timeout(std::time::Duration::from_secs(10), request).await.ok()?.ok()?;
                            let item = t3_core::timeline::decode_detail(value).ok()??;
                            (!t3_core::thread::is_work_active(&item.status)).then(|| (item_id, Arc::new(item)))
                        }
                    });
                    let items: std::collections::HashMap<String, Arc<t3_core::turn_item::TurnItem>> =
                        futures::future::join_all(fetches).await.into_iter().flatten().collect();
                    items
                })
                .await
                .unwrap_or_default();

            if settled.is_empty() {
                return;
            }
            let _ = this.update(cx, |state, cx| {
                if state.connection != connection {
                    return;
                }
                let is_selected = state.selected_thread.as_deref() == Some(thread_id.as_str());
                let Some(thread) = state.thread_state_mut(&thread_id) else { return };
                if thread.deleted {
                    return;
                }
                if thread.settle_turn_items(&thread_id, &settled) {
                    cx.emit(if is_selected { AppEvent::Thread } else { AppEvent::SideThread });
                }
            });
        })
        .detach();
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
}

pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn pending_drop_matches(
    key: &t3_core::sidebar::tag_filter::ThreadKey,
    active_env: Option<&str>,
    thread_id: &str,
) -> bool {
    key.thread_id == thread_id && (key.environment_id.is_empty() || active_env == Some(key.environment_id.as_str()))
}

impl AppState {
    /// A sidebar drag step; `persist` once the drag settles, as the web writes on pointer up.
    pub fn set_sidebar_width(&mut self, width: f32, persist: bool) {
        self.sidebar_width = width;
        if persist {
            store::save_ui_state(t3_core::sidebar::layout::STORAGE_KEY, &width);
        }
    }

    /// Whether the sidebar is out of sight, collapsed or a closed sheet; hidden views stop ticking.
    pub fn sidebar_hidden(&self) -> bool {
        self.sidebar_sheet.hidden(self.sidebar.collapsed)
    }

    pub fn set_sidebar_viewport(&mut self, viewport_width: f32, cx: &mut Context<Self>) {
        if self.sidebar_sheet.set_viewport(viewport_width) {
            cx.emit(AppEvent::SidebarVisibility);
        }
    }

    /// `mod+b` and the toggle: the sheet below the breakpoint, the docked sidebar above it.
    pub fn toggle_sidebar_or_sheet(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_sheet.toggle() {
            cx.emit(AppEvent::SidebarVisibility);
        } else {
            self.toggle_sidebar(cx);
        }
    }

    pub fn close_sidebar_sheet(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_sheet.close() {
            cx.emit(AppEvent::SidebarVisibility);
        }
    }

    /// Rail double-click: forget the stored width and return to the default.
    pub fn reset_sidebar_width(&mut self) {
        self.sidebar_width = t3_core::sidebar::layout::preferred_width(None);
        store::save_ui_state(t3_core::sidebar::layout::STORAGE_KEY, &Value::Null);
    }
}

#[cfg(all(test, feature = "snapshot"))]
#[path = "scoped_route_tests.rs"]
mod scoped_route_tests;

#[cfg(all(test, feature = "snapshot"))]
mod stream_and_stop_tests {
    use super::*;
    use t3_client::supervisor::{ClientEvent, ClientFixture, ConnectionStatus};
    use t3_core::commands::method;

    #[gpui::test]
    async fn stream_error_emits_thread_update_and_recovers_on_next_thread_item(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, _fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let emits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let emits_log = emits.clone();
        let _sub = state.update(cx, |_, cx| {
            cx.subscribe(&state, move |_, _, event, _| {
                if matches!(event, AppEvent::Thread) {
                    emits_log.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        });

        state.update(cx, |state, cx| {
            state.select_thread("t1", cx);
        });
        let before = emits.load(std::sync::atomic::Ordering::SeqCst);

        state.update(cx, |state, cx| {
            state.apply_batch(
                vec![ClientEvent::StreamError {
                    context: "thread t1".into(),
                    message: "stream boom".into(),
                }],
                cx,
            );
        });
        state.read_with(cx, |state, _| {
            assert_eq!(state.last_error.as_deref(), Some("Could not read thread t1: stream boom"));
        });
        assert!(emits.load(std::sync::atomic::Ordering::SeqCst) > before, "StreamError must emit AppEvent::Thread");

        let before_recovery = emits.load(std::sync::atomic::Ordering::SeqCst);
        let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
            "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
            "projection": {
                "thread": {
                    "id": "t1", "projectId": "checkout-a", "title": "T1",
                    "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                    "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                    "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                    "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
                },
                "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
                "providerThreads": [], "providerTurns": [], "runtimeRequests": [],
                "messages": [], "plans": [], "turnItems": [], "checkpointScopes": [],
                "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                "visibleTurnItems": [], "updatedAt": "2026-04-01T00:00:05.000Z"
            }
        }"#).unwrap();
        state.update(cx, |state, cx| {
            state.apply_batch(
                vec![ClientEvent::Thread { thread_id: "t1".into(), item: Box::new(snapshot) }],
                cx,
            );
        });
        state.read_with(cx, |state, _| {
            assert_eq!(state.last_error, None, "valid thread item must clear thread stream error");
        });
        assert!(emits.load(std::sync::atomic::Ordering::SeqCst) > before_recovery);
    }

    #[gpui::test]
    async fn stop_thread_queries_get_turn_item_and_settles_stale_background_work(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let (state, _old_fixture) = cx.update(|cx| crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone()));
        let (client, mut requests) = ClientFixture::handle(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            })).unwrap(),
        );
        let shell = state.read_with(cx, |state, _| state.shell.clone());
        state.update(cx, |state, cx| {
            let mut shell = shell;
            shell.snapshot.as_mut().unwrap().threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t1", "projectId": "checkout-a", "title": "T1",
                "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"},
                "status": "idle", "hasActionableProposedPlan": false, "itemCount": 1, "visibleItemCount": 1
            })).unwrap()];
            state.install_active_environment(client, shell, state.server_config.clone(), ConnectionStatus::Connected, cx);
            state.select_thread("t1", cx);
            let snapshot: t3_core::thread::ThreadStreamItem = serde_json::from_str(r#"{
                "kind": "snapshot", "snapshotSequence": 1, "hasMoreHistory": false, "historyCursor": null,
                "projection": {
                    "thread": {
                        "id": "t1", "projectId": "checkout-a", "title": "T1",
                        "createdAt": "2026-04-01T00:00:00.000Z", "updatedAt": "2026-04-01T00:00:05.000Z",
                        "archivedAt": null, "branch": null, "worktreePath": null, "lastVisitedAt": null,
                        "providerInstanceId": "replay", "modelSelection": {"instanceId":"replay","model":"replay"},
                        "runtimeMode": "auto", "interactionMode": "default", "lineage": {"rootThreadId":"t1"}
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
                    "messages": [], "plans": [],
                    "turnItems": [{
                        "id": "cmd-1", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                        "ordinal": 2, "type": "command_execution", "status": "running", "title": null,
                        "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                        "input": "sleep 5", "output": ""
                    }],
                    "checkpointScopes": [], "checkpoints": [], "contextHandoffs": [], "contextTransfers": [],
                    "visibleTurnItems": [{
                        "position": 0, "visibility": "local", "sourceThreadId": "t1", "sourceItemId": "cmd-1",
                        "item": {
                            "id": "cmd-1", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                            "ordinal": 2, "type": "command_execution", "status": "running", "title": null,
                            "startedAt": null, "completedAt": null, "updatedAt": "2026-04-01T00:00:01.000Z",
                            "input": "sleep 5", "output": ""
                        }
                    }],
                    "updatedAt": "2026-04-01T00:00:05.000Z"
                }
            }"#).unwrap();
            state.thread.apply(snapshot);
            cx.emit(AppEvent::Thread);
        });

        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel();
        let settled_tx = Arc::new(std::sync::Mutex::new(Some(settled_tx)));
        let _sub = state.update(cx, |_, cx| {
            let settled_tx = settled_tx.clone();
            cx.subscribe(&state, move |state, _, event, _| {
                if matches!(event, AppEvent::Thread)
                    && state.thread.projection.as_ref().is_some_and(|p| p.turn_items[0].status == "interrupted")
                {
                    if let Some(tx) = settled_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        });

        let responder = runtime.spawn(async move {
            let (stop_method, stop_payload, stop_reply) = loop {
                let req = requests.next_request().await;
                if req.0 == method::DISPATCH_COMMAND && req.1["type"] == "thread.visit" {
                    let _ = req.2.send(Ok(serde_json::json!({"sequence": 1})));
                    continue;
                }
                break req;
            };
            assert_eq!(stop_method, method::DISPATCH_COMMAND);
            assert_ne!(stop_payload["type"], "thread.visit");
            let _ = stop_reply.send(Ok(serde_json::json!({})));

            let (item_method, item_payload, item_reply) = requests.next_request().await;
            assert_eq!(item_method, t3_core::timeline::GET_TURN_ITEM);
            assert_eq!(item_payload["threadId"], "t1");
            assert_eq!(item_payload["itemId"], "cmd-1");
            let _ = item_reply.send(Ok(serde_json::json!({
                "item": {
                    "id": "cmd-1", "threadId": "t1", "runId": "r1", "nodeId": null, "parentItemId": null,
                    "ordinal": 2, "type": "command_execution", "status": "interrupted", "title": null,
                    "startedAt": null, "completedAt": "2026-04-01T00:00:06.000Z",
                    "updatedAt": "2026-04-01T00:00:06.000Z", "input": "sleep 5", "output": ""
                }
            })));
        });

        state.update(cx, |state, cx| {
            state.stop_thread("t1", cx);
        });

        responder.await.unwrap();
        settled_rx.await.unwrap();
        state.read_with(cx, |state, _| {
            let p = state.thread.projection.as_ref().unwrap();
            assert_eq!(p.turn_items[0].status, "interrupted");
            assert_eq!(p.visible_turn_items[0].item.status, "interrupted");
        });
    }
}
