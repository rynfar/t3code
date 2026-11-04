//! Saved environments and how the app reaches one: pairing with a link, with T3 Code on this
//! Mac, or over SSH, switching, disconnecting, removing, retrying, and the re-pair prompt after a
//! server rejects this device. Every enabled saved environment stays connected; selection
//! attaches the active view to its existing supervisor. The pairing, discovery and SSH work runs on the network runtime (`t3_client::local`,
//! `auth`, `ssh`). SSH follows the desktop's registry (`client-runtime/src/connection/registry.ts`):
//! removing or disconnecting an SSH environment closes its tunnel and stops the remote server
//! this app started; the SSH password prompts queue here for `ssh_prompt.rs`.

use std::sync::Arc;

use gpui::Context;
use t3_client::auth::{self, Credentials, Paired};
use t3_client::local::{self, LocalServer};
use t3_client::ssh::{DiscoveredSshHost, PasswordRequest, SshPairing, SshTarget};
use t3_client::store::{self, SavedEnvironment};
use t3_client::supervisor::Prepare;
use t3_core::pairing::parse_pairing_link;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::toast::{Toast, ToastKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingSource {
    /// "Connect to T3 Code on this Mac": `t3 pair` mints the token.
    Local,
    /// A pasted pairing link.
    Link,
    /// An SSH host: the app starts T3 Code there and pairs over SSH.
    Ssh,
}

/// A saved environment whose server stopped accepting this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: String,
    pub label: String,
    pub reason: String,
    pub local: bool,
    /// Reached over SSH, which can pair it again.
    pub ssh: bool,
}

/// What the connect screen shows besides the saved list.
#[derive(Debug, Clone, Default)]
pub struct ConnectScreen {
    /// Open over a live connection ("Switch environment…").
    pub switching: bool,
    pub pairing: Option<PairingSource>,
    pub error: Option<(PairingSource, String)>,
    pub rejected: Option<Rejected>,
    pub local_server: Option<LocalServer>,
    pub discovering: bool,
    /// Hosts from `~/.ssh/config` and `known_hosts`; `None` until first read.
    pub ssh_hosts: Option<Vec<DiscoveredSshHost>>,
    pub ssh_discovering: bool,
    /// Counts pairings started or cancelled; a result for an earlier one is dropped.
    attempt: u64,
    /// The pairing running on the network runtime. Aborting it stops `t3 pair`.
    pairing_task: Option<tokio::task::AbortHandle>,
}

impl AppState {
    pub fn active_environment(&self) -> Option<&SavedEnvironment> {
        self.environments.active()
    }

    /// The saved entry for T3 Code on this Mac, if it was paired before.
    pub fn local_saved_environment(&self) -> Option<&SavedEnvironment> {
        let server = self.connect.local_server.as_ref()?;
        self.environments.find(server.environment_id(), &server.state.origin)
    }

    /// Looks for T3 Code running on this Mac, off the UI thread.
    pub fn refresh_local_server(&mut self, cx: &mut Context<Self>) {
        if self.connect.discovering {
            return;
        }
        self.connect.discovering = true;
        cx.emit(AppEvent::Phase);
        let task = self.runtime().spawn(local::discover());
        cx.spawn(async move |this, cx| {
            let found = task.await.ok().flatten();
            match &found {
                Some(server) => tracing::info!(origin = %server.state.origin, version = server.server_version().unwrap_or("unknown"), "found T3 Code on this Mac"),
                None => tracing::info!("T3 Code is not running on this Mac"),
            }
            let _ = this.update(cx, |state, cx| {
                state.connect.discovering = false;
                state.connect.local_server = found;
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    pub fn pair_link(&mut self, link: &str, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let link = match parse_pairing_link(link, None) {
            Ok(link) => link,
            Err(error) => return self.fail_pairing(PairingSource::Link, error.to_string(), cx),
        };
        let task = self.runtime().spawn(async move { auth::pair_with_link(&link).await.map_err(|error| error.to_string()) });
        self.await_pairing(PairingSource::Link, task, |state, paired, cx| state.adopt(paired, false, cx), cx);
    }

    /// "Connect to T3 Code on this Mac": reuses a saved credential for it, else pairs.
    pub fn connect_local(&mut self, cx: &mut Context<Self>) {
        match self.local_saved_environment().filter(|environment| environment.access_token.is_some()).map(|environment| environment.id.clone()) {
            Some(id) => self.connect_environment(&id, cx),
            None => self.pair_local(cx),
        }
    }

    /// Mints a fresh pairing with `t3 pair`, even when one is saved.
    pub fn pair_local(&mut self, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let Some(server) = self.connect.local_server.clone() else {
            return self.fail_pairing(PairingSource::Local, local::LocalPairError::NotRunning.to_string(), cx);
        };
        let task = self.runtime().spawn(async move { local::pair(&server).await.map_err(|error| error.to_string()) });
        self.await_pairing(PairingSource::Local, task, |state, paired, cx| state.adopt(paired, true, cx), cx);
    }

    /// Reads the SSH config and known hosts again, off the UI thread, each time the SSH form
    /// shows. The last list stays while it refreshes.
    pub fn refresh_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        if self.connect.ssh_discovering {
            return;
        }
        self.connect.ssh_discovering = true;
        cx.emit(AppEvent::Phase);
        let task = self.runtime().spawn_blocking(t3_client::ssh::discover_hosts);
        cx.spawn(async move |this, cx| {
            let hosts = task.await.unwrap_or_default();
            let _ = this.update(cx, |state, cx| {
                state.connect.ssh_discovering = false;
                state.connect.ssh_hosts = Some(hosts);
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }

    /// The discovered hosts no saved SSH environment reaches yet.
    pub fn unsaved_ssh_hosts(&self) -> Vec<DiscoveredSshHost> {
        let saved: Vec<SshTarget> = self.environments.environments.iter().filter_map(|environment| environment.ssh.clone()).collect();
        t3_core::ssh::unsaved_hosts(self.connect.ssh_hosts.as_deref().unwrap_or_default(), &saved)
    }

    /// "Add environment" over SSH: starts T3 Code on the host, pairs over SSH, saves the
    /// environment, and connects it.
    pub fn add_ssh(&mut self, target: SshTarget, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let ssh = self.ssh.clone();
        let task = self.runtime().spawn(async move { ssh.provision(&target, None).await.map_err(|error| error.to_string()) });
        self.await_pairing(PairingSource::Ssh, task, |state, pairing, cx| state.adopt_ssh(pairing, cx), cx);
    }

    /// Pairs a saved SSH environment again over SSH, refusing a host that now reaches another
    /// machine.
    pub fn pair_ssh_again(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let Some(environment) = self.environments.get(id).cloned() else { return };
        let Some(target) = environment.ssh else { return };
        let (ssh, expected) = (self.ssh.clone(), environment.environment_id);
        let task = self.runtime().spawn(async move { ssh.provision(&target, expected.as_deref()).await.map_err(|error| error.to_string()) });
        self.await_pairing(PairingSource::Ssh, task, |state, pairing, cx| state.adopt_ssh(pairing, cx), cx);
    }

    fn await_pairing<T: Send + 'static>(
        &mut self,
        source: PairingSource,
        task: tokio::task::JoinHandle<Result<T, String>>,
        adopt: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.connect.attempt += 1;
        let attempt = self.connect.attempt;
        self.connect.pairing = Some(source);
        self.connect.pairing_task = Some(task.abort_handle());
        self.connect.error = None;
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                // A cancelled attempt can finish before its abort lands; it changes nothing.
                if state.connect.attempt != attempt {
                    return;
                }
                let result = result.unwrap_or_else(|_| Err("Pairing stopped unexpectedly. Try again.".to_owned()));
                state.connect.pairing = None;
                state.connect.pairing_task = None;
                match result {
                    Ok(paired) => {
                        tracing::info!(?source, "paired");
                        adopt(state, paired, cx);
                    }
                    Err(message) => {
                        tracing::warn!(%message, ?source, "pairing failed");
                        state.fail_pairing(source, message, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn fail_pairing(&mut self, source: PairingSource, message: String, cx: &mut Context<Self>) {
        self.connect.error = Some((source, message));
        cx.emit(AppEvent::Phase);
    }

    /// Stops a pairing in flight, `t3 pair` included, and drops its result.
    fn cancel_pairing(&mut self) {
        if let Some(task) = self.connect.pairing_task.take() {
            task.abort();
        }
        self.connect.attempt += 1;
        self.connect.pairing = None;
    }

    /// The connect screen as a new connection leaves it, still knowing the local server and the
    /// SSH hosts.
    fn reset_connect_screen(&mut self) {
        self.cancel_pairing();
        let (local_server, ssh_hosts, attempt) = (self.connect.local_server.take(), self.connect.ssh_hosts.take(), self.connect.attempt);
        self.connect = ConnectScreen { local_server, ssh_hosts, attempt, ..Default::default() };
    }

    /// Saves a new pairing and connects to it.
    fn adopt(&mut self, paired: Paired, local: bool, cx: &mut Context<Self>) {
        let id = self.environments.upsert(&paired, local);
        self.persist_environments();
        self.reset_connect_screen();
        self.reset_connection(cx);
        self.connections.remove(&id);
        self.retire_environment_notifications(&id, cx);
        self.start_environment(&id, cx);
        self.connections.activate(Some(&id));
    }

    /// Saves a pairing made over SSH and connects to it through its tunnel.
    fn adopt_ssh(&mut self, pairing: SshPairing, cx: &mut Context<Self>) {
        let alias = pairing.target.alias.clone();
        let id = self.environments.upsert_ssh(&pairing.paired, pairing.target);
        self.persist_environments();
        self.reset_connect_screen();
        self.reset_connection(cx);
        self.connections.remove(&id);
        self.retire_environment_notifications(&id, cx);
        self.start_environment(&id, cx);
        self.connections.activate(Some(&id));
        self.show_toast(Toast::new(ToastKind::Success, "Environment connected").description(format!("{alias} is ready over an SSH-managed tunnel.")), cx);
    }

    /// Starts the client for a saved environment with a credential. An SSH environment connects
    /// through its tunnel on this session's port, which the supervisor readies before each attempt.
    pub(crate) fn start_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.environments.get(id).cloned() else { return };
        let Some(access_token) = environment.access_token.clone() else { return };
        let Some(target) = environment.ssh.clone() else {
            return self.connections.watch(id, Credentials { http_base: environment.http_base, access_token }, None);
        };
        let port = match self.ssh.session_port(&target) {
            Ok(port) => port,
            Err(error) => return self.fail_pairing(PairingSource::Ssh, format!("Could not reserve a local port for the SSH tunnel: {error}"), cx),
        };
        let http_base = format!("http://127.0.0.1:{port}");
        if self.environments.relocate(id, &http_base) {
            self.persist_environments();
        }
        let (ssh, expected, address) = (self.ssh.clone(), environment.environment_id, http_base.clone());
        let prepare: Prepare = Arc::new(move || {
            let (ssh, target, expected, address) = (ssh.clone(), target.clone(), expected.clone(), address.clone());
            Box::pin(async move { ssh.prepare(&target, expected.as_deref(), &address).await })
        });
        self.connections.watch(id, Credentials { http_base, access_token }, Some(prepare));
    }

    /// The SSH tunnel had to move to another local port, and its client stopped. Reconnects
    /// there once the event pump that reported it has returned.
    pub(crate) fn environment_route_moved(&mut self, id: &str, http_base: String, cx: &mut Context<Self>) {
        if self.environments.get(id).is_none_or(|e| e.disabled) { return; }
        self.environments.relocate(id, &http_base);
        self.persist_environments();
        self.start_environment(id, cx);
        if self.environments.active.as_deref() == Some(id) {
            self.reset_connection(cx);
            self.connections.activate(Some(id));
        }
    }

    /// Connects a saved environment, or asks to pair it again when its credential was rejected.
    /// T3 Code on this Mac is reached where it runs now, which moves when its port does.
    pub fn connect_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.notification_route.as_ref().is_some_and(|target| target.environment_id != id) { self.notification_route = None; }
        let moved = self.relocate_local(id);
        let Some(environment) = self.environments.get(id).cloned() else { return };
        if !moved && self.environments.active.as_deref() == Some(id) && self.client_handle().is_some() {
            return self.close_switcher(cx);
        }
        if environment.access_token.is_none() {
            if environment.local && self.local_saved_environment().is_some_and(|local| local.id == environment.id) {
                return self.pair_local(cx);
            }
            if environment.ssh.is_some() {
                return self.pair_ssh_again(&environment.id, cx);
            }
            let message = format!("Paste a new pairing link from {} to connect it again.", environment.label);
            return self.fail_pairing(PairingSource::Link, message, cx);
        }
        if let Some(entry) = self.environments.environments.iter_mut().find(|e| e.id == id) { entry.disabled = false; }
        self.environments.active = Some(environment.id.clone());
        self.persist_environments();
        self.reset_connect_screen();
        self.reset_connection(cx);
        self.start_environment(&environment.id, cx);
        self.connections.activate(Some(&environment.id));
    }

    /// Points a saved local pairing at the address the running server recorded, when discovery
    /// found the same server elsewhere. Returns whether it moved.
    fn relocate_local(&mut self, id: &str) -> bool {
        let Some(origin) = self.connect.local_server.as_ref().map(|server| server.state.origin.clone()) else { return false };
        let same_server = self.local_saved_environment().is_some_and(|saved| saved.id == id && saved.local);
        if !same_server || !self.environments.relocate(id, &origin) {
            return false;
        }
        tracing::info!(%origin, "T3 Code on this Mac moved");
        self.persist_environments();
        true
    }

    /// Ends the connection and shows the connect screen. The environment stays saved, and the
    /// next launch waits here instead of reconnecting.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        let ssh = self.active_environment().and_then(|environment| environment.ssh.clone());
        self.cancel_pairing();
        if let Some(id) = self.environments.active.clone() {
            if let Some(entry) = self.environments.environments.iter_mut().find(|e| e.id == id) { entry.disabled = true; }
            self.connections.remove(&id);
            self.environment_statuses.remove(&id);
            self.retire_environment_notifications(&id, cx);
        }
        self.connections.activate(None);
        self.notification_route = None;
        self.environments.active = None;
        self.persist_environments();
        self.reset_connection(cx);
        self.connect.switching = false;
        self.set_phase(Phase::Unpaired, cx);
        self.refresh_local_server(cx);
        if let Some(target) = ssh {
            self.stop_ssh(target);
        }
    }

    /// Closes an SSH environment's tunnel and stops the remote server this app started, in the
    /// background. The supervisor that used it is already gone.
    fn stop_ssh(&self, target: SshTarget) {
        let ssh = self.ssh.clone();
        self.runtime().spawn(async move {
            if let Err(error) = ssh.disconnect(&target).await {
                tracing::warn!(%error, alias = %target.alias, "could not disconnect the managed SSH environment");
            }
        });
    }

    /// "Remove from this device": forgets the environment and its credential.
    pub fn remove_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let was_active = self.environments.active.as_deref() == Some(id) && self.client_handle().is_some();
        let ssh = self.environments.get(id).and_then(|environment| environment.ssh.clone());
        if !self.environments.remove(id) {
            return;
        }
        self.persist_environments();
        self.connections.remove(id);
        self.environment_statuses.remove(id);
        self.retire_environment_notifications(id, cx);
        if self.notification_route.as_ref().is_some_and(|key| key.environment_id == id) { self.notification_route = None; }
        self.connect.rejected = self.connect.rejected.take().filter(|rejected| rejected.id != id);
        if was_active {
            self.reset_connection(cx);
            self.connect.switching = false;
            self.set_phase(Phase::Unpaired, cx);
        }
        if let Some(target) = ssh {
            self.stop_ssh(target);
        }
        cx.emit(AppEvent::Phase);
    }

    pub fn open_switcher(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Online(_)) {
            self.connect.switching = true;
            self.connect.error = None;
        }
        cx.emit(AppEvent::Phase);
        self.refresh_local_server(cx);
    }

    pub fn close_switcher(&mut self, cx: &mut Context<Self>) {
        self.cancel_pairing();
        self.connect.switching = false;
        self.connect.error = None;
        cx.emit(AppEvent::Phase);
    }

    /// Skips the reconnect wait, or checks a blocked server again.
    pub fn retry_connection(&self) {
        if let Some(client) = self.client_handle() {
            client.retry_now();
        }
    }

    /// The server rejected this device: drop the credential, keep the entry, and prompt to pair
    /// again. Runs inside the event pump, which ends on its own once the client stops.
    pub(crate) fn environment_credentials_rejected(&mut self, id: &str, reason: String, cx: &mut Context<Self>) {
        let rejected = self.environments.get(id).map(|environment| Rejected {
            id: id.into(), label: environment.label.clone(), reason, local: environment.local, ssh: environment.ssh.is_some(),
        });
        self.environments.revoke(id);
        self.persist_environments();
        self.connections.remove(id);
        self.environment_statuses.remove(id);
        self.retire_environment_notifications(id, cx);
        if self.environments.active.as_deref() != Some(id) { cx.emit(AppEvent::Phase); return; }
        self.notification_route = None;
        self.connect.rejected = rejected;
        self.connect.switching = false;
        self.clear_connection_state(cx);
        self.set_phase(Phase::Unpaired, cx);
    }

    /// Keeps the saved label, id, and version in step with what the server reports.
    pub(crate) fn sync_environment_identity(&mut self) {
        let (Some(config), Some(id)) = (self.server_config.config.as_ref(), self.environments.active.clone()) else { return };
        let environment = &config.environment;
        if self.environments.update_identity(&id, &environment.environment_id, &environment.label, &environment.server_version) {
            self.persist_environments();
        }
    }

    /// Queues an SSH password prompt for the dialog.
    pub(crate) fn queue_ssh_prompt(&mut self, request: PasswordRequest, cx: &mut Context<Self>) {
        self.ssh_prompt_sequence += 1;
        self.ssh_prompts.push_back((self.ssh_prompt_sequence, request));
        cx.emit(AppEvent::SshPrompt);
    }

    /// Takes the shown SSH prompt to answer or dismiss it.
    pub(crate) fn take_ssh_prompt(&mut self, sequence: u64, cx: &mut Context<Self>) -> Option<PasswordRequest> {
        if self.ssh_prompts.front().is_none_or(|(front, _)| *front != sequence) {
            return None;
        }
        let (_, request) = self.ssh_prompts.pop_front()?;
        cx.emit(AppEvent::SshPrompt);
        Some(request)
    }

    /// Hands the edits since the last save to the store's writer, off the UI thread. It applies
    /// them to the file as it is then, so another instance's entries survive.
    pub(crate) fn persist_environments(&mut self) {
        store::save_environment_changes(self.environments.changes_since(&self.environments_saved));
        self.environments_saved = self.environments.clone();
    }

    /// Stops the connection and drops everything that belonged to it.
    fn reset_connection(&mut self, cx: &mut Context<Self>) {
        self.clear_connection_state(cx);
    }
}
