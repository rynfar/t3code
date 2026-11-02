//! Saved environments and how the app reaches one: pairing with a link or with T3 Code on this
//! Mac, switching, disconnecting, removing, retrying, and the re-pair prompt after a server
//! rejects this device. One environment is connected at a time; the saved list holds the rest.
//! The pairing and discovery work runs on the network runtime (`t3_client::local`, `auth`).

use gpui::Context;
use t3_client::auth::{self, Paired};
use t3_client::local::{self, LocalServer};
use t3_client::store::{self, SavedEnvironment};
use t3_core::pairing::parse_pairing_link;

use crate::app_state::{AppEvent, AppState, Phase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingSource {
    /// "Connect to T3 Code on this Mac": `t3 pair` mints the token.
    Local,
    /// A pasted pairing link.
    Link,
}

/// A saved environment whose server stopped accepting this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub id: String,
    pub label: String,
    pub reason: String,
    pub local: bool,
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
        self.await_pairing(PairingSource::Link, task, cx);
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
        self.await_pairing(PairingSource::Local, task, cx);
    }

    fn await_pairing(&mut self, source: PairingSource, task: tokio::task::JoinHandle<Result<Paired, String>>, cx: &mut Context<Self>) {
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
                        tracing::info!(origin = %paired.credentials.http_base, ?source, "paired");
                        state.adopt(paired, source == PairingSource::Local, cx);
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

    fn fail_pairing(&mut self, source: PairingSource, message: String, cx: &mut Context<Self>) {
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

    /// The connect screen as a new connection leaves it, still knowing the local server.
    fn reset_connect_screen(&mut self) {
        self.cancel_pairing();
        let (local_server, attempt) = (self.connect.local_server.take(), self.connect.attempt);
        self.connect = ConnectScreen { local_server, attempt, ..Default::default() };
    }

    /// Saves a new pairing and connects to it.
    fn adopt(&mut self, paired: Paired, local: bool, cx: &mut Context<Self>) {
        self.environments.upsert(&paired, local);
        self.persist_environments();
        self.reset_connect_screen();
        self.reset_connection(cx);
        self.start_client(paired.credentials, cx);
    }

    /// Connects a saved environment, or asks to pair it again when its credential was rejected.
    /// T3 Code on this Mac is reached where it runs now, which moves when its port does.
    pub fn connect_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let moved = self.relocate_local(id);
        let Some(environment) = self.environments.get(id).cloned() else { return };
        if !moved && self.environments.active.as_deref() == Some(id) && self.client_handle().is_some() {
            return self.close_switcher(cx);
        }
        let Some(credentials) = environment.credentials() else {
            if environment.local && self.local_saved_environment().is_some_and(|local| local.id == environment.id) {
                return self.pair_local(cx);
            }
            let message = format!("Paste a new pairing link from {} to connect it again.", environment.label);
            return self.fail_pairing(PairingSource::Link, message, cx);
        };
        self.environments.active = Some(environment.id);
        self.persist_environments();
        self.reset_connect_screen();
        self.reset_connection(cx);
        self.start_client(credentials, cx);
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
        self.cancel_pairing();
        self.environments.active = None;
        self.persist_environments();
        self.reset_connection(cx);
        self.connect.switching = false;
        self.set_phase(Phase::Unpaired, cx);
        self.refresh_local_server(cx);
    }

    /// "Remove from this device": forgets the environment and its credential.
    pub fn remove_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let was_active = self.environments.active.as_deref() == Some(id) && self.client_handle().is_some();
        if !self.environments.remove(id) {
            return;
        }
        self.persist_environments();
        self.connect.rejected = self.connect.rejected.take().filter(|rejected| rejected.id != id);
        if was_active {
            self.reset_connection(cx);
            self.connect.switching = false;
            self.set_phase(Phase::Unpaired, cx);
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
    pub(crate) fn credentials_rejected(&mut self, reason: String, cx: &mut Context<Self>) {
        tracing::warn!(%reason, "the server rejected this device's credential");
        let rejected = self.environments.active().map(|environment| Rejected {
            id: environment.id.clone(),
            label: environment.label.clone(),
            reason,
            local: environment.local,
        });
        if let Some(rejected) = &rejected {
            self.environments.revoke(&rejected.id);
            self.persist_environments();
        }
        self.connect.rejected = rejected;
        self.connect.switching = false;
        self.clear_connection_state(cx);
        self.set_phase(Phase::Unpaired, cx);
        self.refresh_local_server(cx);
    }

    /// Keeps the saved label, id, and version in step with what the server reports.
    pub(crate) fn sync_environment_identity(&mut self) {
        let (Some(config), Some(id)) = (self.server_config.config.as_ref(), self.environments.active.clone()) else { return };
        let environment = &config.environment;
        if self.environments.update_identity(&id, &environment.environment_id, &environment.label, &environment.server_version) {
            self.persist_environments();
        }
    }

    /// Hands the edits since the last save to the store's writer, off the UI thread. It applies
    /// them to the file as it is then, so another instance's entries survive.
    fn persist_environments(&mut self) {
        store::save_environment_changes(self.environments.changes_since(&self.environments_saved));
        self.environments_saved = self.environments.clone();
    }

    /// Stops the connection and drops everything that belonged to it.
    fn reset_connection(&mut self, cx: &mut Context<Self>) {
        self.stop_pump();
        self.clear_connection_state(cx);
    }
}
