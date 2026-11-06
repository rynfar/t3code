//! Environment-keyed supervisors, as `connection/registry.ts` and
//! `ThreadNotificationCoordinator.tsx` watch every enabled environment. Shell reduction and
//! notification gates stay on Tokio; background frames never enter the UI queue.

use std::collections::HashMap;
use std::sync::Arc;

use t3_core::auth::AuthSessionState;
use t3_core::sidebar::fleet::EnvironmentSummary;
use t3_core::sidebar::grouping::EnvironmentOwner;

use t3_core::notifications::{
    EnvironmentNotifications, LinkedEnvironment, LinkedRequests, NotificationVisibility, ThreadNotification,
};
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use t3_core::sidebar::tag_filter::{self, ThreadKey};
use tokio::sync::{mpsc, watch};

use crate::auth::Credentials;
use crate::supervisor::{self, ClientEvent, ClientHandle, ConnectionStatus, Prepare};


#[derive(Debug, Clone)]
pub struct EnvironmentView {
    pub summary: Arc<EnvironmentSummary>,
    pub status: ConnectionStatus,
    pub config: ServerConfigState,
    pub grants: Option<AuthSessionState>,
    pub settings_revision: u64,
    pub keybindings_revision: u64,
}

#[derive(Debug, Clone, Default)]
pub struct FleetView {
    pub revision: u64,
    pub environments: Vec<Arc<EnvironmentView>>,
}

impl FleetView {
    pub fn environment(&self, id: &str) -> Option<&EnvironmentView> {
        self.environments.iter().find(|environment| environment.summary.owner.saved_id == id).map(Arc::as_ref)
    }
}

pub struct Destination {
    pub owner: EnvironmentOwner,
    pub client: ClientHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationError { Missing, Stale, Disconnected, Unsynchronized, Denied }

impl Destination {
    pub fn is_current(&self, view: &FleetView) -> bool {
        view.environment(&self.owner.saved_id).is_some_and(|environment| environment.summary.owner == self.owner
            && environment.status == ConnectionStatus::Connected && environment.summary.synchronized)
            && *self.client.connection_generation().borrow() == self.owner.connection_generation
    }
}

enum RelayFrame { Events(Vec<ClientEvent>), Permission }

pub enum EnvironmentEvent {
    Activated {
        id: String,
        client: ClientHandle,
        shell: Box<ShellState>,
        config: Box<ServerConfigState>,
        status: ConnectionStatus,
        generation: u64,
    },
    Active {
        id: String,
        generation: u64,
        events: Vec<ClientEvent>,
    },
    Status {
        id: String,
        generation: u64,
        status: ConnectionStatus,
    },
    Moved {
        id: String,
        generation: u64,
        http_base: String,
    },
    Identity {
        id: String,
        generation: u64,
        descriptor: Box<t3_core::server_config::EnvironmentDescriptor>,
    },
    Visibility(NotificationVisibility),
    Notifications {
        id: String,
        generation: u64,
        notifications: Vec<ThreadNotification>,
    },
}

enum Command {
    #[cfg(feature = "test-support")]
    Fixture { id: String, generation: u64, client: ClientHandle, shell: ShellState, config: ServerConfigState, status: ConnectionStatus, reply: tokio::sync::oneshot::Sender<mpsc::Sender<ClientEvent>> },
    ApplySettings { owner: EnvironmentOwner, settings: t3_core::server_config::ServerSettings, revision: u64, reply: tokio::sync::oneshot::Sender<Result<bool, DestinationError>> },
    ApplyKeybindings { owner: EnvironmentOwner, value: serde_json::Value, revision: u64, reply: tokio::sync::oneshot::Sender<Result<bool, DestinationError>> },
    Destination { owner: EnvironmentOwner, scope: String, reply: tokio::sync::oneshot::Sender<Result<Destination, DestinationError>> },
    Client { id: String, reply: tokio::sync::oneshot::Sender<Option<ClientHandle>> },
    Watch {
        id: String,
        generation: u64,
        credentials: Credentials,
        prepare: Option<Prepare>,
    },
    Activate(Option<String>),
    Remove(String),
    Enabled(bool),
}

pub struct Environments {
    commands: mpsc::UnboundedSender<Command>,
    task: tokio::task::JoinHandle<()>,
    leases: HashMap<String, (Credentials, u64)>,
    generation: u64,
    view: watch::Receiver<Arc<FleetView>>,
}

impl Drop for Environments {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Environments {
    pub fn start(
        runtime: &tokio::runtime::Handle,
        enabled: bool,
    ) -> (Self, mpsc::Receiver<EnvironmentEvent>) {
        let (commands, rx) = mpsc::unbounded_channel();
        let (events, output) = mpsc::channel(128);
        let (view_tx, view) = watch::channel(Arc::new(FleetView::default()));
        let task = runtime.spawn(run(rx, events, enabled, view_tx));
        (
            Self {
                commands,
                task,
                leases: HashMap::new(),
                generation: 0,
                view,
            },
            output,
        )
    }

    #[cfg(feature = "test-support")]
    pub fn install_fixture(&mut self, id: &str, client: ClientHandle, shell: ShellState, config: ServerConfigState, status: ConnectionStatus) -> impl std::future::Future<Output = mpsc::Sender<ClientEvent>> + Send + 'static + use<> {
        self.generation += 1;
        self.leases.insert(id.into(), (Credentials { http_base: "http://fixture.invalid".into(), access_token: String::new() }, self.generation));
        let (reply, response) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(Command::Fixture { id: id.into(), generation: self.generation, client, shell, config, status, reply });
        async move { response.await.expect("fixture environment owner stopped") }
    }

    pub fn watch(&mut self, id: &str, credentials: Credentials, prepare: Option<Prepare>) {
        if self
            .leases
            .get(id)
            .is_some_and(|(current, _)| current == &credentials)
        {
            return;
        }
        self.generation += 1;
        self.leases
            .insert(id.into(), (credentials.clone(), self.generation));
        let _ = self.commands.send(Command::Watch {
            id: id.into(),
            generation: self.generation,
            credentials,
            prepare,
        });
    }
    pub fn is_current(&self, event: &EnvironmentEvent) -> bool {
        let (id, generation) = match event {
            EnvironmentEvent::Activated { id, generation, .. }
            | EnvironmentEvent::Active { id, generation, .. }
            | EnvironmentEvent::Status { id, generation, .. }
            | EnvironmentEvent::Moved { id, generation, .. }
            | EnvironmentEvent::Identity { id, generation, .. }
            | EnvironmentEvent::Notifications { id, generation, .. } => (id, generation),
            EnvironmentEvent::Visibility(_) => return true,
        };
        self.leases
            .get(id)
            .is_some_and(|(_, current)| current == generation)
    }
    pub fn client(&self, id: &str) -> impl std::future::Future<Output = Option<ClientHandle>> + Send + 'static + use<> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(Command::Client { id: id.into(), reply });
        async move { response.await.ok().flatten() }
    }

    pub fn apply_settings_reply(&self, owner: &EnvironmentOwner, settings: t3_core::server_config::ServerSettings, sent_revision: u64) -> impl std::future::Future<Output = Result<bool, DestinationError>> + Send + 'static + use<> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(Command::ApplySettings { owner: owner.clone(), settings, revision: sent_revision, reply });
        async move { response.await.unwrap_or(Err(DestinationError::Missing)) }
    }

    pub fn apply_keybindings_reply(&self, owner: &EnvironmentOwner, value: serde_json::Value, sent_revision: u64) -> impl std::future::Future<Output = Result<bool, DestinationError>> + Send + 'static + use<> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(Command::ApplyKeybindings { owner: owner.clone(), value, revision: sent_revision, reply });
        async move { response.await.unwrap_or(Err(DestinationError::Missing)) }
    }

    pub fn view(&self) -> watch::Receiver<Arc<FleetView>> { self.view.clone() }

    pub fn destination(&self, owner: &EnvironmentOwner, scope: &str) -> impl std::future::Future<Output = Result<Destination, DestinationError>> + Send + 'static + use<> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(Command::Destination { owner: owner.clone(), scope: scope.into(), reply });
        async move { response.await.unwrap_or(Err(DestinationError::Missing)) }
    }

    pub fn activate(&self, id: Option<&str>) {
        let _ = self.commands.send(Command::Activate(id.map(str::to_owned)));
    }
    pub fn remove(&mut self, id: &str) {
        self.leases.remove(id);
        let _ = self.commands.send(Command::Remove(id.into()));
    }
    pub fn notifications_enabled(&self, enabled: bool) {
        let _ = self.commands.send(Command::Enabled(enabled));
    }
}

struct Entry {
    client: ClientHandle,
    credentials: Credentials,
    relay: tokio::task::JoinHandle<()>,
    generation: u64,
    shell: ShellState,
    config: ServerConfigState,
    status: ConnectionStatus,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.relay.abort();
    }
}


impl Entry {
    fn owner(&self, id: &str) -> EnvironmentOwner {
        EnvironmentOwner { saved_id: id.into(), canonical_id: self.config.config.as_ref().map(|config| config.environment.environment_id.clone()).filter(|id| !id.is_empty()), generation: self.generation, connection_generation: *self.client.connection_generation().borrow(), shell_generation: self.shell.generation }
    }

    fn destination(&self, owner: &EnvironmentOwner, scope: &str) -> Result<Destination, DestinationError> {
        if self.owner(&owner.saved_id) != *owner { return Err(DestinationError::Stale); }
        if self.status != ConnectionStatus::Connected { return Err(DestinationError::Disconnected); }
        if !self.shell.synchronized { return Err(DestinationError::Unsynchronized); }
        if !self.client.session_state().borrow().as_ref().is_some_and(|session| session.grants(scope)) { return Err(DestinationError::Denied); }
        Ok(Destination { owner: owner.clone(), client: self.client.clone() })
    }
}

fn apply_shell(entry: &mut Entry, item: &t3_core::shell::ShellStreamItem) -> bool {
    let quiet = matches!(item, t3_core::shell::ShellStreamItem::ThreadUpdated { location, thread, .. }
        if location == "active" && entry.shell.thread(&thread.id).is_some_and(|prior| t3_core::sidebar::thread::same_apart_from_streaming(prior, thread)));
    entry.shell.apply(item.clone()) && !quiet
}

fn publish(entries: &HashMap<String, Entry>, changed: &str, material: bool, output: &watch::Sender<Arc<FleetView>>) {
    let previous = output.borrow().clone();
    let mut environments = previous.environments.iter().filter(|environment| entries.contains_key(&environment.summary.owner.saved_id)).cloned().collect::<Vec<_>>();
    let mut dirty = environments.len() != previous.environments.len();
    let mut relink = material || dirty;
    if let Some(entry) = entries.get(changed) {
        let owner = entry.owner(changed);
        let grants = entry.client.session_state().borrow().clone();
        let index = environments.iter().position(|environment| environment.summary.owner.saved_id == changed);
        let prior = index.map(|index| &environments[index]);
        let summary_config = |state: &ServerConfigState| state.config.as_ref().map(|config| (config.environment.label.clone(), config.scratch_workspace_root.clone()));
        let summary = match prior.filter(|prior| !material && prior.summary.owner == owner && summary_config(&prior.config) == summary_config(&entry.config)) {
            Some(prior) => prior.summary.clone(),
            None => { relink = true; Arc::new(EnvironmentSummary::from_shell(owner, &entry.shell, &entry.config)) },
        };
        let same = prior.is_some_and(|prior| prior.summary == summary && prior.status == entry.status && prior.config.config == entry.config.config && prior.grants == grants && prior.settings_revision == entry.config.settings_revision() && prior.keybindings_revision == entry.config.keybindings_revision());
        if !same {
            let view = Arc::new(EnvironmentView { summary, status: entry.status.clone(), config: entry.config.clone(), grants, settings_revision: entry.config.settings_revision(), keybindings_revision: entry.config.keybindings_revision() });
            match index { Some(index) => environments[index] = view, None => environments.push(view) }
            dirty = true;
        }
    }
    if relink {
        let linked_environments = entries.iter().filter(|(_, entry)| entry.status == ConnectionStatus::Connected && entry.shell.synchronized)
            .filter_map(|(id, entry)| Some(LinkedEnvironment { saved_id: id, canonical_id: entry.config.config.as_ref().map(|config| config.environment.environment_id.as_str()).filter(|id| !id.is_empty()).unwrap_or(id), threads: &entry.shell.snapshot.as_ref()?.threads })).collect::<Vec<_>>();
        let linked = LinkedRequests::build(&linked_environments);
        for environment in &mut environments {
            let requests = linked.requests.get(&environment.summary.owner.saved_id);
            let changed = environment.summary.threads.iter().any(|thread| thread.thread.pending_subagent_request != requests.and_then(|requests| requests.get(&thread.thread.id)).copied());
            if changed {
                let mut summary = (*environment.summary).clone();
                for thread in &mut summary.threads { thread.thread.pending_subagent_request = requests.and_then(|requests| requests.get(&thread.thread.id)).copied(); }
                let mut view = (**environment).clone(); view.summary = Arc::new(summary); *environment = Arc::new(view);
                dirty = true;
            }
        }
    }
    if dirty {
        environments.sort_by(|a, b| a.summary.owner.saved_id.cmp(&b.summary.owner.saved_id));
        output.send_replace(Arc::new(FleetView { revision: previous.revision + 1, environments }));
    }
}

async fn relay_events(
    mut events: mpsc::Receiver<ClientEvent>,
    mut grants: watch::Receiver<Option<AuthSessionState>>,
    mut connections: watch::Receiver<u64>,
    tx: mpsc::Sender<(String, u64, RelayFrame)>,
    key: String,
    epoch: u64,
) {
    loop {
        let frame = tokio::select! {
            event = events.recv() => {
                let Some(first) = event else { break };
                let mut batch = vec![first];
                while batch.len() < 512 {
                    match events.try_recv() { Ok(event) => batch.push(event), Err(_) => break }
                }
                RelayFrame::Events(batch)
            }
            result = grants.changed() => { if result.is_err() { break; } RelayFrame::Permission }
            result = connections.changed() => { if result.is_err() { break; } RelayFrame::Permission }
        };
        if tx.send((key.clone(), epoch, frame)).await.is_err() { break; }
    }
}

fn visibility(entries: &HashMap<String, Entry>, linked: &LinkedRequests) -> NotificationVisibility {
    let rows: Vec<_> = entries
        .iter()
        .map(|(id, entry)| {
            let canonical = entry
                .config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            let threads = entry
                .shell
                .snapshot
                .iter()
                .flat_map(|s| s.threads.iter())
                .collect::<Vec<_>>();
            let tags = entry
                .config
                .config
                .as_ref()
                .and_then(|c| c.settings.hidden_tags());
            (id, canonical, threads, tags)
        })
        .collect();
    let filtering = rows.iter().any(|(_, _, _, tags)| tags.is_some());
    let environments: Vec<_> = rows
        .iter()
        .map(|(_, id, threads, tags)| tag_filter::Environment {
            environment_id: id,
            threads,
            hidden_tags: tags.as_deref(),
        })
        .collect();
    let hidden = tag_filter::hidden_thread_keys(
        &environments,
        &Default::default(),
        &Default::default(),
        filtering,
    );
    let mut result = NotificationVisibility {
        filtering,
        delegated_under_parent: linked.delegated_under_parent.clone(),
        ..Default::default()
    };
    for (saved, canonical, threads, _) in rows {
        for thread in threads {
            let key = ThreadKey::new(saved, &thread.id);
            if hidden.contains(&ThreadKey::new(canonical, &thread.id)) {
                result.hidden.insert(key.clone());
            }
            result.live.insert(key);
        }
    }
    result
}

async fn reobserve(
    entries: &HashMap<String, Entry>, watchers: &mut EnvironmentNotifications,
    visible: &mut NotificationVisibility, output: &mpsc::Sender<EnvironmentEvent>, enabled: bool,
) {
    let environments = entries.iter().filter_map(|(id, entry)| {
        if entry.status != ConnectionStatus::Connected || !entry.shell.synchronized { return None; }
        let snapshot = entry.shell.snapshot.as_ref()?;
        let canonical_id = entry.config.config.as_ref().map(|c| c.environment.environment_id.as_str()).filter(|id| !id.is_empty()).unwrap_or(id);
        Some(LinkedEnvironment { saved_id: id, canonical_id, threads: &snapshot.threads })
    }).collect::<Vec<_>>();
    let linked = LinkedRequests::build(&environments);
    let next = visibility(entries, &linked);
    if next != *visible { *visible = next; let _ = output.send(EnvironmentEvent::Visibility(visible.clone())).await; }
    if !enabled { return; }
    for environment in environments {
        let entry = &entries[environment.saved_id];
        let notifications = watchers.observe_linked(environment.saved_id, &entry.shell, visible, &linked);
        if !notifications.is_empty() {
            let _ = output.send(EnvironmentEvent::Notifications { id: environment.saved_id.into(), generation: entry.generation, notifications }).await;
        }
    }
}

async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    output: mpsc::Sender<EnvironmentEvent>,
    mut enabled: bool,
    view: watch::Sender<Arc<FleetView>>,
) {
    let (frames, mut input) = mpsc::channel::<(String, u64, RelayFrame)>(128);
    let mut entries: HashMap<String, Entry> = HashMap::new();
    let mut active = None;
    let mut watchers = EnvironmentNotifications::default();
    let mut visible = NotificationVisibility::default();
    loop {
        tokio::select! {
            _ = output.closed() => return,
            command = commands.recv() => match command {
                None => return,
                #[cfg(feature = "test-support")]
                Some(Command::Fixture { id, generation, client, shell, config, status, reply }) => {
                    let (tx, events) = mpsc::channel(128);
                    let relay = tokio::spawn(relay_events(events, client.session_state(), client.connection_generation(), frames.clone(), id.clone(), generation));
                    let credentials = Credentials { http_base: "http://fixture.invalid".into(), access_token: String::new() };
                    entries.insert(id.clone(), Entry { client, credentials, relay, generation, shell, config, status });
                    publish(&entries, &id, true, &view);
                    reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await;
                    let _ = reply.send(tx);
                }
                Some(Command::ApplySettings { owner, settings, revision, reply }) => {
                    let result = entries.get_mut(&owner.saved_id).ok_or(DestinationError::Missing).and_then(|entry| {
                        if entry.owner(&owner.saved_id) != owner { return Err(DestinationError::Stale); }
                        Ok(entry.config.apply_settings_reply(settings, revision))
                    });
                    if result == Ok(true) { publish(&entries, &owner.saved_id, false, &view); }
                    let _ = reply.send(result);
                }
                Some(Command::ApplyKeybindings { owner, value, revision, reply }) => {
                    let result = entries.get_mut(&owner.saved_id).ok_or(DestinationError::Missing).and_then(|entry| {
                        if entry.owner(&owner.saved_id) != owner { return Err(DestinationError::Stale); }
                        Ok(entry.config.apply_keybindings_reply(&value, revision))
                    });
                    if result == Ok(true) { publish(&entries, &owner.saved_id, false, &view); }
                    let _ = reply.send(result);
                }
                Some(Command::Destination { owner, scope, reply }) => { let _ = reply.send(entries.get(&owner.saved_id).ok_or(DestinationError::Missing).and_then(|entry| entry.destination(&owner, &scope))); }
                Some(Command::Client { id, reply }) => { let _ = reply.send(entries.get(&id).map(|entry| entry.client.clone())); }
                Some(Command::Watch { id, generation, credentials, prepare }) => {
                    if entries.get(&id).is_some_and(|e| e.credentials == credentials) { continue; }
                    let prior = entries.remove(&id);
                    let (client, events) = supervisor::start_prepared(&tokio::runtime::Handle::current(), credentials.clone(), prepare);
                    let (tx, key, epoch) = (frames.clone(), id.clone(), generation);
                    let relay = tokio::spawn(relay_events(events, client.session_state(), client.connection_generation(), tx, key, epoch));
                    let mut shell = prior.as_ref().map(|e| e.shell.clone()).unwrap_or_default();
                    shell.synchronized = false;
                    let config = prior.as_ref().map(|e| e.config.clone()).unwrap_or_default();
                    entries.insert(id.clone(), Entry { client, credentials, relay, generation, shell, config, status: ConnectionStatus::Connecting });
                    publish(&entries, &id, true, &view);
                    tracing::info!(environment = %id, watched = entries.len(), "watching environment with one supervisor");
                    reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await;
                }
                Some(Command::Activate(id)) => {
                    if active != id && let Some(prior) = active.as_ref().and_then(|id| entries.get(id)) { prior.client.close_all_threads(); }
                    active = id;
                    if let Some(id) = &active && let Some(entry) = entries.get(id) {
                        let _ = output.send(EnvironmentEvent::Activated { id: id.clone(), client: entry.client.clone(), shell: Box::new(entry.shell.clone()), config: Box::new(entry.config.clone()), status: entry.status.clone(), generation: entry.generation }).await;
                    }
                }
                Some(Command::Remove(id)) => {
                    entries.remove(&id);
                    watchers.remove(&id);
                    publish(&entries, &id, true, &view);
                    if active.as_ref() == Some(&id) { active = None; }
                    reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await;
                }
                Some(Command::Enabled(next)) => {
                    if next == enabled { continue; }
                    watchers.clear(); enabled = next;
                    reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await;
                }
            },
            frame = input.recv() => {
                let Some((id, epoch, frame)) = frame else { return };
                if !entries.get(&id).is_some_and(|entry| entry.generation == epoch) { continue; }
                let batch = match frame {
                    RelayFrame::Events(batch) => batch,
                    RelayFrame::Permission => { publish(&entries, &id, false, &view); continue; }
                };
                let entry = entries.get_mut(&id).unwrap();
                let mut changed = false;
                let mut config_changed = false;
                let revisions = (entry.config.settings_revision(), entry.config.keybindings_revision());
                let mut lifecycle_changed = false;
                for event in &batch {
                    match event {
                        ClientEvent::Shell(item) => {
                            changed |= apply_shell(entry, item);
                        }
                        ClientEvent::ServerConfig(event) => { config_changed |= entry.config.apply(*event.clone()); }
                        ClientEvent::Status(status) => {
                            lifecycle_changed |= entry.status != *status;
                            entry.status = status.clone();
                            if *status != ConnectionStatus::Connected { entry.shell.synchronized = false; }
                            let _ = output.send(EnvironmentEvent::Status { id: id.clone(), generation: epoch, status: status.clone() }).await;
                        }
                        ClientEvent::Moved { http_base } => { lifecycle_changed = true; let _ = output.send(EnvironmentEvent::Moved { id: id.clone(), generation: epoch, http_base: http_base.clone() }).await; }
                        _ => {}
                    }
                }
                if config_changed && let Some(config) = &entry.config.config {
                    let _ = output.send(EnvironmentEvent::Identity { id: id.clone(), generation: epoch, descriptor: Box::new(config.environment.clone()) }).await;
                }
                if changed || config_changed || lifecycle_changed || revisions != (entry.config.settings_revision(), entry.config.keybindings_revision()) {
                    publish(&entries, &id, changed || lifecycle_changed, &view);
                    if changed || config_changed || lifecycle_changed { reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await; }
                }
                if active.as_ref() == Some(&id) {
                    let events = batch.into_iter().filter(|e| !matches!(e, ClientEvent::Status(_) | ClientEvent::Moved { .. })).collect::<Vec<_>>();
                    if !events.is_empty() { let _ = output.send(EnvironmentEvent::Active { id, generation: epoch, events }).await; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
