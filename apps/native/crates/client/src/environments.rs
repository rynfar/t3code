//! Environment-keyed supervisors, as `connection/registry.ts` and
//! `ThreadNotificationCoordinator.tsx` watch every enabled environment. Shell reduction and
//! notification gates stay on Tokio; background frames never enter the UI queue.

use std::collections::HashMap;

use t3_core::notifications::{
    EnvironmentNotifications, LinkedEnvironment, LinkedRequests, NotificationVisibility, ThreadNotification,
};
use t3_core::server_config::ServerConfigState;
use t3_core::shell::ShellState;
use t3_core::sidebar::tag_filter::{self, ThreadKey};
use tokio::sync::mpsc;

use crate::auth::Credentials;
use crate::supervisor::{self, ClientEvent, ClientHandle, ConnectionStatus, Prepare};

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
        let task = runtime.spawn(run(rx, events, enabled));
        (
            Self {
                commands,
                task,
                leases: HashMap::new(),
                generation: 0,
            },
            output,
        )
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
) {
    let (frames, mut input) = mpsc::channel::<(String, u64, Vec<ClientEvent>)>(128);
    let mut entries: HashMap<String, Entry> = HashMap::new();
    let mut active = None;
    let mut watchers = EnvironmentNotifications::default();
    let mut visible = NotificationVisibility::default();
    loop {
        tokio::select! {
            _ = output.closed() => return,
            command = commands.recv() => match command {
                None => return,
                Some(Command::Client { id, reply }) => { let _ = reply.send(entries.get(&id).map(|entry| entry.client.clone())); }
                Some(Command::Watch { id, generation, credentials, prepare }) => {
                    if entries.get(&id).is_some_and(|e| e.credentials == credentials) { continue; }
                    let prior = entries.remove(&id);
                    let (client, mut events) = supervisor::start_prepared(&tokio::runtime::Handle::current(), credentials.clone(), prepare);
                    let (tx, key, epoch) = (frames.clone(), id.clone(), generation);
                    let relay = tokio::spawn(async move {
                        while let Some(first) = events.recv().await {
                            let mut batch = vec![first];
                            while batch.len() < 512 {
                                match events.try_recv() { Ok(event) => batch.push(event), Err(_) => break }
                            }
                            if tx.send((key.clone(), epoch, batch)).await.is_err() { break; }
                        }
                    });
                    let mut shell = prior.as_ref().map(|e| e.shell.clone()).unwrap_or_default();
                    shell.synchronized = false;
                    let config = prior.as_ref().map(|e| e.config.clone()).unwrap_or_default();
                    entries.insert(id.clone(), Entry { client, credentials, relay, generation, shell, config, status: ConnectionStatus::Connecting });
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
                let Some((id, epoch, batch)) = frame else { return };
                let Some(entry) = entries.get_mut(&id).filter(|e| e.generation == epoch) else { continue };
                let mut changed = false;
                let mut config_changed = false;
                let mut lifecycle_changed = false;
                for event in &batch {
                    match event {
                        ClientEvent::Shell(item) => {
                            let quiet = matches!(item, t3_core::shell::ShellStreamItem::ThreadUpdated { location, thread, .. }
                                if location == "active" && entry.shell.thread(&thread.id).is_some_and(|prior| t3_core::sidebar::thread::same_apart_from_streaming(prior, thread)));
                            let applied = entry.shell.apply(item.clone());
                            changed |= applied && !quiet;
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
                if changed || config_changed || lifecycle_changed {
                    reobserve(&entries, &mut watchers, &mut visible, &output, enabled).await;
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
