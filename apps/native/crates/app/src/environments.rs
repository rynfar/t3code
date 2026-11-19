//! Saved environments and how the app reaches one: pairing with a link, with T3 Code on this
//! Mac, or over SSH, switching, disconnecting, removing, retrying, and the re-pair prompt after a
//! server rejects this device. Every enabled saved environment stays connected; selection
//! attaches the active view to its existing supervisor. The pairing, discovery and SSH work runs on the network runtime (`t3_client::local`,
//! `auth`, `ssh`). SSH follows the desktop's registry (`client-runtime/src/connection/registry.ts`):
//! removing or disconnecting an SSH environment closes its tunnel and stops the remote server
//! this app started; the SSH password prompts queue here for `ssh_prompt.rs`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use gpui::Context;
use t3_client::auth::{self, Credentials, Paired};
use t3_client::load_balancing::{
    CandidateProvider, DEFAULT_LOAD_BALANCING_WEIGHT, DraftEnvironmentCandidate,
    HostResourcesSnapshot, attachments_block_auto_routing_warning,
    eligible_load_balancing_candidates, fetch_candidate_host_resources,
    load_balancing_send_warning, refresh_load_balanced_selection,
};
use t3_client::local::{self, LocalServer};
use t3_client::routes::CandidateRoute;
use t3_client::ssh::{DiscoveredSshHost, PasswordRequest, SshPairing, SshTarget};
use t3_client::store::{self, SavedEnvironment};
use t3_client::supervisor::{ConnectionStatus, Prepare};
use t3_core::pairing::{PairingCredentialResult, PeerPairingCodeError, parse_pairing_link};

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
    /// Rejected request's trace ID when the server reported one (`pingdotgg/t3code#38041`).
    pub trace_id: Option<String>,
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
        self.pair_link_with_code(link, None, cx);
    }

    /// Pairs with a full pairing URL, a hosted pairing link, a `t3code://` link, or a host + separate code.
    pub fn pair_link_with_code(&mut self, link: &str, code: Option<&str>, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let link = match parse_pairing_link(link, code) {
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
    /// environment, and connects it. Untargeted onboarding guards saved environment routes so an
    /// already-saved environment is not silently given a new SSH route (`pingdotgg/t3code#37818`).
    pub fn add_ssh(&mut self, target: SshTarget, cx: &mut Context<Self>) {
        if self.connect.pairing.is_some() {
            return;
        }
        let ssh = self.ssh.clone();
        let task = self.runtime().spawn(async move { ssh.provision(&target, None).await.map_err(|error| error.to_string()) });
        self.await_pairing(PairingSource::Ssh, task, |state, pairing, cx| state.adopt_ssh(pairing, true, cx), cx);
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
        self.await_pairing(PairingSource::Ssh, task, |state, pairing, cx| state.adopt_ssh(pairing, false, cx), cx);
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
    fn adopt_ssh(&mut self, pairing: SshPairing, untargeted: bool, cx: &mut Context<Self>) {
        if untargeted
            && let Err(message) = self.environments.check_ssh_onboarding_conflict(
                pairing.paired.descriptor.as_ref().map(|d| d.environment_id.as_str()),
                &pairing.target,
            )
        {
            self.stop_ssh(pairing.target);
            self.fail_pairing(PairingSource::Ssh, message, cx);
            return;
        }
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
    /// Direct environments pass any persisted alternate routes for multi-route candidate fallback.
    pub(crate) fn start_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.environments.get(id).cloned() else { return };
        let Some(access_token) = environment.access_token.clone() else { return };
        let Some(target) = environment.ssh.clone() else {
            let alternate_routes = store::load_alternate_routes().remove(id).unwrap_or_default();
            return self.connections.watch_with_routes(
                id,
                Credentials { http_base: environment.http_base, access_token },
                alternate_routes,
                environment.environment_id,
                None,
            );
        };
        let port = match self.ssh.session_port(&target) {
            Ok(port) => port,
            Err(error) => return self.fail_pairing(PairingSource::Ssh, format!("Could not reserve a local port for the SSH tunnel: {error}"), cx),
        };
        let http_base = format!("http://127.0.0.1:{port}");
        if self.environments.relocate(id, &http_base) {
            self.persist_environments();
        }
        let (ssh, expected, address) = (self.ssh.clone(), environment.environment_id.clone(), http_base.clone());
        let prepare: Prepare = Arc::new(move || {
            let (ssh, target, expected, address) = (ssh.clone(), target.clone(), expected.clone(), address.clone());
            Box::pin(async move { ssh.prepare(&target, expected.as_deref(), &address).await })
        });
        self.connections.watch_with_routes(
            id,
            Credentials { http_base, access_token },
            Vec::new(),
            environment.environment_id,
            Some(prepare),
        );
    }

    /// Persists alternate/learned direct routes for a saved environment (`CN-SESSION`).
    pub fn record_environment_alternate_routes(&self, id: &str, routes: Vec<CandidateRoute>) {
        let mut all = store::load_alternate_routes();
        if routes.is_empty() {
            if all.remove(id).is_some() {
                store::save_alternate_routes(&all);
            }
            return;
        }
        if all.get(id) == Some(&routes) {
            return;
        }
        all.insert(id.to_owned(), routes);
        store::save_alternate_routes(&all);
    }

    /// Merges server-advertised direct endpoints into the persisted route table for each saved
    /// environment so subsequent startups can fall back across learned routes (`CN-SESSION`).
    pub fn sync_learned_environment_routes(&self) {
        for env in &self.environments.environments {
            if env.ssh.is_some() {
                continue;
            }
            let reported = t3_client::supervisor::server_config_extras_for(&env.http_base)
                .borrow()
                .direct_endpoint_entries
                .clone();
            if reported.is_empty() {
                continue;
            }
            let existing = store::load_alternate_routes().remove(&env.id).unwrap_or_default();
            let primary = CandidateRoute::saved(env.http_base.clone());
            let mut current = vec![primary.clone()];
            current.extend(existing);
            let env_id = env.environment_id.as_deref().unwrap_or(&env.id);
            if let Some(updated) = t3_client::routes::merge_learned_routes(env_id, &current, &primary.id, &reported, true) {
                let alternates = updated
                    .into_iter()
                    .filter(|candidate| candidate.http_base.trim_end_matches('/') != env.http_base.trim_end_matches('/'))
                    .collect();
                self.record_environment_alternate_routes(&env.id, alternates);
            }
        }
    }

    /// Resolves connected credentials for `environment_id` (matching canonical `environmentId` or
    /// saved ID) if this device's session is connected and authorized to create pairing codes.
    fn peer_pairing_credentials(&self, environment_id: &str) -> Result<Credentials, PeerPairingCodeError> {
        for view in &self.fleet.environments {
            let canonical = view
                .config
                .config
                .as_ref()
                .map(|cfg| cfg.environment.environment_id.as_str())
                .filter(|id| !id.is_empty());
            if view.summary.owner.saved_id != environment_id && canonical != Some(environment_id) {
                continue;
            }
            if view.status != ConnectionStatus::Connected {
                return Err(PeerPairingCodeError::unavailable(Some("The environment is not connected.".to_owned())));
            }
            if view.grants.as_ref().is_some_and(|grants| !grants.grants("access:write")) {
                return Err(PeerPairingCodeError::not_allowed(None));
            }
            return self
                .environments
                .get(&view.summary.owner.saved_id)
                .and_then(SavedEnvironment::credentials)
                .ok_or_else(|| PeerPairingCodeError::unavailable(Some("Missing credentials.".to_owned())));
        }
        if let Some(active) = self.active_environment() {
            let matches_active = active.id == environment_id
                || active.environment_id.as_deref() == Some(environment_id)
                || self
                    .server_config
                    .config
                    .as_ref()
                    .is_some_and(|cfg| cfg.environment.environment_id == environment_id);
            if matches_active {
                if !matches!(self.phase, Phase::Online(ConnectionStatus::Connected)) {
                    return Err(PeerPairingCodeError::unavailable(Some("The environment is not connected.".to_owned())));
                }
                if self
                    .client_handle()
                    .and_then(|c| c.session_state().borrow().clone())
                    .is_some_and(|grants| !grants.grants("access:write"))
                {
                    return Err(PeerPairingCodeError::not_allowed(None));
                }
                return active
                    .credentials()
                    .ok_or_else(|| PeerPairingCodeError::unavailable(Some("Missing credentials.".to_owned())));
            }
        }
        Err(PeerPairingCodeError::unavailable(Some("The environment is not connected.".to_owned())))
    }

    /// Whether this client is currently connected to `environment_id` with a session authorized to
    /// mint one-use pairing codes (`access:write`) on a host that supports peer-link codes (`pingdotgg/t3code#37825`).
    pub fn can_mint_pairing_code_on(&self, environment_id: &str) -> bool {
        let Ok(creds) = self.peer_pairing_credentials(environment_id) else {
            return false;
        };
        auth::descriptor_capabilities(&creds.http_base)
            .is_none_or(|caps| caps.peer_link_pairing_codes)
    }

    /// `mintPairingCodeOn` (`apps/web/src/state/peerPairingCode.ts`):
    /// mints a fresh one-use pairing code on `environment_id` using this client's session there.
    pub fn mint_pairing_code_on(
        &self,
        environment_id: &str,
        access: &str,
    ) -> impl Future<Output = Result<String, PeerPairingCodeError>> + Send + 'static + use<> {
        let resolved = self.peer_pairing_credentials(environment_id);
        let access = access.to_owned();
        let runtime = self.runtime().clone();
        async move {
            let credentials = resolved?;
            runtime
                .spawn(async move { auth::mint_peer_pairing_code(&credentials, &access).await })
                .await
                .map_err(|error| PeerPairingCodeError::unavailable(Some(error.to_string())))?
        }
    }

    /// `mintMcpApprovalCodeOn` (`apps/web/src/state/peerPairingCode.ts`):
    /// mints a fresh one-use code on `environment_id` that approves one app's sign-in at `access`.
    pub fn mint_mcp_approval_code_on(
        &self,
        environment_id: &str,
        access: &str,
    ) -> impl Future<Output = Result<PairingCredentialResult, PeerPairingCodeError>> + Send + 'static + use<> {
        let resolved = self.peer_pairing_credentials(environment_id);
        let access = access.to_owned();
        let runtime = self.runtime().clone();
        async move {
            let credentials = resolved?;
            runtime
                .spawn(async move { auth::mint_mcp_approval_code(&credentials, &access).await })
                .await
                .map_err(|error| PeerPairingCodeError::unavailable(Some(error.to_string())))?
        }
    }

    /// Collects connected fleet environments hosting a logical project's `(saved_id, project_id)`
    /// registrations that are eligible for automatic load balancing (`useDraftEnvironmentRouting.ts`).
    pub fn load_balancing_candidates_for_project_envs(
        &self,
        project_envs: &[(String, String)],
        selected_provider_driver: Option<&str>,
        active_provider_instance_id: Option<&str>,
        referenced_thread_environment_ids: &[String],
        weights: &HashMap<String, f64>,
    ) -> Vec<DraftEnvironmentCandidate> {
        let mut raw = Vec::new();
        for (saved_id, project_id) in project_envs {
            let Some(view) = self.fleet.environment(saved_id) else {
                continue;
            };
            let canonical_id = view
                .config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .or_else(|| {
                    self.environments
                        .get(saved_id)
                        .and_then(|saved| saved.environment_id.clone())
                })
                .unwrap_or_else(|| saved_id.clone());
            let weight = weights
                .get(&canonical_id)
                .or_else(|| weights.get(saved_id))
                .copied()
                .unwrap_or(DEFAULT_LOAD_BALANCING_WEIGHT);
            let providers = view
                .config
                .config
                .as_ref()
                .map(|cfg| {
                    cfg.providers
                        .iter()
                        .map(|provider| CandidateProvider {
                            instance_id: provider.instance_id.clone(),
                            driver: provider.driver.clone(),
                            enabled: t3_core::providers::instance_enabled(
                                &cfg.settings,
                                Some(project_id.as_str()),
                                &provider.instance_id,
                            ),
                            installed: provider.installed,
                            status: provider.status.clone(),
                            auth_status: (!provider.auth.status.is_empty())
                                .then(|| provider.auth.status.clone()),
                            availability: provider.availability.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            raw.push(DraftEnvironmentCandidate {
                saved_id: saved_id.clone(),
                environment_id: canonical_id,
                project_id: Some(project_id.clone()),
                connected: view.status == ConnectionStatus::Connected,
                weight,
                providers,
            });
        }
        match selected_provider_driver {
            Some(driver) if !driver.trim().is_empty() => eligible_load_balancing_candidates(
                &raw,
                referenced_thread_environment_ids,
                driver,
                active_provider_instance_id,
            ),
            _ => raw
                .into_iter()
                .filter(|candidate| {
                    candidate.connected
                        && candidate.project_id.is_some()
                        && candidate.weight.is_finite()
                        && candidate.weight > 0.0
                        && referenced_thread_environment_ids
                            .iter()
                            .all(|env_id| env_id == &candidate.environment_id)
                })
                .collect(),
        }
    }

    /// Concurrently queries `server.getHostResources` across `candidates` on the network runtime.
    /// Candidates whose supervisor client is unavailable settle with `None` so callers record the
    /// failure instead of staying pending indefinitely.
    pub fn fetch_load_balancing_resources(
        &self,
        candidates: &[DraftEnvironmentCandidate],
    ) -> impl Future<Output = Vec<(String, Option<HostResourcesSnapshot>, i64)>> + Send + 'static + use<> {
        let active_id = self.environments.active.clone();
        let active_client = self.client_handle();
        let lookups: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                let immediate = (active_id.as_deref() == Some(candidate.saved_id.as_str()))
                    .then(|| active_client.clone())
                    .flatten();
                let lookup = immediate.is_none().then(|| self.connections.client(&candidate.saved_id));
                (candidate.environment_id.clone(), immediate, lookup)
            })
            .collect();
        let runtime = self.runtime().clone();
        async move {
            runtime
                .spawn(async move {
                    let mut ready = Vec::with_capacity(lookups.len());
                    let mut missing = Vec::new();
                    for (env_id, immediate, lookup) in lookups {
                        let client = match immediate {
                            Some(handle) => Some(handle),
                            None => match lookup {
                                Some(fut) => fut.await,
                                None => None,
                            },
                        };
                        match client {
                            Some(handle) => ready.push((env_id, handle)),
                            None => missing.push(env_id),
                        }
                    }
                    let mut out = fetch_candidate_host_resources(ready).await;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    for env_id in missing {
                        out.push((env_id, None, now_ms));
                    }
                    out
                })
                .await
                .unwrap_or_default()
        }
    }

    /// Collects connected fleet environments eligible for automatic load balancing
    /// (`useDraftEnvironmentRouting.ts`).
    pub fn load_balancing_candidates_for(
        &self,
        selected_provider_driver: Option<&str>,
        active_provider_instance_id: Option<&str>,
        referenced_thread_environment_ids: &[String],
        weights: &HashMap<String, f64>,
        project_key: Option<&str>,
    ) -> Vec<DraftEnvironmentCandidate> {
        let mut raw = Vec::new();
        for view in &self.fleet.environments {
            let saved_id = view.summary.owner.saved_id.clone();
            let canonical_id = view
                .config
                .config
                .as_ref()
                .map(|c| c.environment.environment_id.clone())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| saved_id.clone());
            let project_id = project_key.and_then(|wanted| {
                view.summary
                    .projects
                    .iter()
                    .find(|p| p.project.id == wanted || p.project.workspace_root == wanted)
                    .map(|p| p.project.id.clone())
            });
            let weight = weights
                .get(&canonical_id)
                .or_else(|| weights.get(&saved_id))
                .copied()
                .unwrap_or(DEFAULT_LOAD_BALANCING_WEIGHT);
            let providers = view
                .config
                .config
                .as_ref()
                .map(|cfg| {
                    cfg.providers
                        .iter()
                        .map(|provider| CandidateProvider {
                            instance_id: provider.instance_id.clone(),
                            driver: provider.driver.clone(),
                            enabled: t3_core::providers::instance_enabled(
                                &cfg.settings,
                                project_id.as_deref(),
                                &provider.instance_id,
                            ),
                            installed: provider.installed,
                            status: provider.status.clone(),
                            auth_status: (!provider.auth.status.is_empty()).then(|| provider.auth.status.clone()),
                            availability: provider.availability.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            raw.push(DraftEnvironmentCandidate {
                saved_id,
                environment_id: canonical_id,
                project_id,
                connected: view.status == ConnectionStatus::Connected,
                weight,
                providers,
            });
        }
        match selected_provider_driver {
            Some(driver) if !driver.trim().is_empty() => eligible_load_balancing_candidates(
                &raw,
                referenced_thread_environment_ids,
                driver,
                active_provider_instance_id,
            ),
            _ => raw
                .into_iter()
                .filter(|candidate| {
                    candidate.connected
                        && (project_key.is_none() || candidate.project_id.is_some())
                        && candidate.weight.is_finite()
                        && candidate.weight > 0.0
                        && referenced_thread_environment_ids.iter().all(|env_id| env_id == &candidate.environment_id)
                })
                .collect(),
        }
    }

    /// Evaluates host resources across eligible connected environments and routes the draft (or
    /// active environment) to the least-loaded machine (`useLoadBalancedEnvironment.ts` +
    /// `useDraftEnvironmentRouting.ts`).
    #[allow(clippy::too_many_arguments)]
    pub fn route_draft_load_balanced(
        &mut self,
        selected_provider_driver: Option<String>,
        active_provider_instance_id: Option<String>,
        has_attachments: bool,
        referenced_thread_environment_ids: Vec<String>,
        weights: HashMap<String, f64>,
        project_key: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if has_attachments {
            let warn = attachments_block_auto_routing_warning();
            self.show_toast(
                Toast::new(ToastKind::Warning, warn.title)
                    .description(warn.description)
                    .key("load-balancing-attachments"),
                cx,
            );
            return;
        }
        let candidates = self.load_balancing_candidates_for(
            selected_provider_driver.as_deref(),
            active_provider_instance_id.as_deref(),
            &referenced_thread_environment_ids,
            &weights,
            project_key.as_deref(),
        );
        if candidates.is_empty() {
            let warn = load_balancing_send_warning(false, &referenced_thread_environment_ids, &[]);
            self.show_toast(Toast::new(ToastKind::Warning, warn.title).description(warn.description), cx);
            return;
        }
        let candidate_env_ids: Vec<String> = candidates.iter().map(|c| c.environment_id.clone()).collect();
        let active_id = self.environments.active.clone();
        let active_client = self.client_handle();
        let lookups: Vec<_> = candidates
            .iter()
            .map(|candidate| {
                let immediate = (active_id.as_deref() == Some(candidate.saved_id.as_str()))
                    .then(|| active_client.clone())
                    .flatten();
                let lookup = immediate.is_none().then(|| self.connections.client(&candidate.saved_id));
                (
                    candidate.saved_id.clone(),
                    candidate.environment_id.clone(),
                    candidate.project_id.clone(),
                    candidate.weight,
                    immediate,
                    lookup,
                )
            })
            .collect();
        let runtime = self.runtime().clone();
        let task = runtime.spawn(async move {
            let mut query_inputs = Vec::new();
            let mut meta = Vec::new();
            for (saved_id, env_id, project_id, weight, immediate, lookup) in lookups {
                let client = match immediate {
                    Some(handle) => Some(handle),
                    None => match lookup {
                        Some(fut) => fut.await,
                        None => None,
                    },
                };
                if let Some(handle) = client {
                    query_inputs.push((env_id.clone(), handle, weight));
                }
                meta.push((saved_id, env_id, project_id));
            }
            let winner_env_id = refresh_load_balanced_selection(query_inputs).await;
            winner_env_id.and_then(|wanted| meta.into_iter().find(|(_, env_id, _)| env_id == &wanted))
        });
        cx.spawn(async move |this, cx| {
            let winner = task.await.ok().flatten();
            let _ = this.update(cx, |state, cx| {
                if let Some((saved_id, _env_id, project_id)) = winner {
                    let label = state
                        .environments
                        .get(&saved_id)
                        .map(|e| e.label.clone())
                        .unwrap_or_else(|| saved_id.clone());
                    if state.environments.active.as_deref() != Some(saved_id.as_str()) {
                        state.connect_environment(&saved_id, cx);
                    }
                    if let Some(pid) = project_id.filter(|p| !p.is_empty()) {
                        state.start_draft(&pid, cx);
                    }
                    state.show_toast(
                        Toast::new(ToastKind::Info, format!("Auto balance selected {label}")),
                        cx,
                    );
                } else {
                    let warn = load_balancing_send_warning(false, &referenced_thread_environment_ids, &candidate_env_ids);
                    state.show_toast(
                        Toast::new(ToastKind::Warning, warn.title).description(warn.description),
                        cx,
                    );
                }
            });
        })
        .detach();
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

    /// "Remove from this device": forgets the environment and its credential, and withdraws any
    /// active permission-update notice toast for it (`pingdotgg/t3code#38980`).
    pub fn remove_environment(&mut self, id: &str, cx: &mut Context<Self>) {
        let was_active = self.environments.active.as_deref() == Some(id) && self.client_handle().is_some();
        let ssh = self.environments.get(id).and_then(|environment| environment.ssh.clone());
        let canonical_id = self
            .environments
            .get(id)
            .and_then(|environment| environment.environment_id.clone())
            .filter(|cid| !cid.is_empty())
            .or_else(|| {
                self.fleet
                    .environment(id)
                    .and_then(|view| view.config.config.as_ref())
                    .map(|cfg| cfg.environment.environment_id.clone())
                    .filter(|cid| !cid.is_empty())
            });
        if !self.environments.remove(id) {
            return;
        }
        self.withdraw_toast_by_key(&format!("permission-update:{id}"), cx);
        if let Some(cid) = canonical_id
            && cid != id
        {
            self.withdraw_toast_by_key(&format!("permission-update:{cid}"), cx);
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

    /// Retries a specific saved environment's supervisor immediately, even when it is not active.
    pub fn retry_environment(&self, id: &str) {
        if self.environments.active.as_deref() == Some(id)
            && let Some(client) = self.client_handle()
        {
            client.retry_now();
            return;
        }
        let lookup = self.connections.client(id);
        self.runtime().spawn(async move {
            if let Some(client) = lookup.await {
                client.retry_now();
            }
        });
    }

    /// The server rejected this device: drop the credential, keep the entry, and prompt to pair
    /// again. Runs inside the event pump, which ends on its own once the client stops.
    pub(crate) fn environment_credentials_rejected(&mut self, id: &str, reason: String, cx: &mut Context<Self>) {
        let trace_id = t3_client::status::extract_trace_id(&reason).map(str::to_owned);
        let rejected = self.environments.get(id).map(|environment| Rejected {
            id: id.into(),
            label: environment.label.clone(),
            reason,
            trace_id,
            local: environment.local,
            ssh: environment.ssh.is_some(),
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
